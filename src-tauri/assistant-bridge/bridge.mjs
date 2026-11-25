/*
 * The AI assistant's SDK models (specs/assistant-cli-models.md): one question, run on this computer through the
 * model's own SDK, with the app's tools over its MCP endpoint (src-tauri/src/agent.rs).
 *
 *   stdin   one JSON object: { provider: "claude" | "codex" | "gemini", model?, system, prompt, web, mcp: { url } }
 *           (the MCP token comes in the environment, OPENTRADER_MCP_TOKEN, with the model's credentials)
 *   stdout  one JSON event per line:
 *             { t: "status", text }                      what the run is doing (setting up…)
 *             { t: "text", text }                        answer text, as it comes
 *             { t: "tool", id, name, args }              a tool the model calls (args: JSON text)
 *             { t: "tool_done", id, ok, result }         its result (text)
 *             { t: "error", message }                    the run failed
 *             { t: "done" }                              the answer is complete
 *
 * The app (src-tauri/src/assistant_cli.rs) installs this folder with npm in its data folder, starts `node bridge.mjs`
 * for each question and kills it on Stop. The app's own destructive tools wait for the user's Allow in the panel
 * (the MCP endpoint asks the window), so the SDKs' permission prompts are not used: only the app's tools (and web
 * search / fetch) are allowed.
 */
import os from "node:os";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const SERVER = "opentrader";
/** The longest tool result passed on to the panel (the model gets the whole result). */
const MAX_SHOWN = 24_000;

const emit = (event) => process.stdout.write(`${JSON.stringify(event)}\n`);
const cut = (s) => (s.length > MAX_SHOWN ? `${s.slice(0, MAX_SHOWN)}\n…(cut: ${s.length - MAX_SHOWN} more characters)` : s);
/** An error's message; an API error given as JSON (perhaps JSON in JSON, as Gemini's) is unwrapped to its message. */
export function errorText(e) {
  let text = e instanceof Error ? e.message : String(e);
  for (let i = 0; i < 3; i++) {
    try {
      const inner = JSON.parse(text)?.error?.message;
      if (typeof inner !== "string") break;
      text = inner;
    } catch {
      break;
    }
  }
  return text;
}

/** The text of MCP / API content blocks. */
export function textOf(content) {
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return content == null ? "" : JSON.stringify(content);
  return content
    .map((c) => (c?.type === "text" ? c.text : c?.type === "image" ? "[picture]" : JSON.stringify(c)))
    .join("\n");
}

/** A tool's name without the MCP server's prefix (`mcp__opentrader__get_bars` → `get_bars`). */
export const shortName = (name) => String(name ?? "").replace(/^mcp__opentrader__/, "");

/** An empty folder to run in: the models get no project files. */
function workDir() {
  const dir = path.join(os.tmpdir(), "opentrader-assistant");
  fs.mkdirSync(dir, { recursive: true });
  return dir;
}

// ── Claude Code (Claude Agent SDK) ───────────────────────────────────────
async function runClaude(input, signal) {
  const { query } = await import("@anthropic-ai/claude-agent-sdk");
  const abortController = new AbortController();
  signal.addEventListener("abort", () => abortController.abort(), { once: true });
  const web = input.web ? ["WebSearch", "WebFetch"] : [];
  const allowed = (name) => name.startsWith(`mcp__${SERVER}__`) || web.includes(name);
  const q = query({
    prompt: input.prompt,
    options: {
      model: input.model || "claude-opus-5-5",
      systemPrompt: input.system,
      // No shell, no files: the app's tools and the web only.
      tools: web,
      mcpServers: {
        [SERVER]: { type: "http", url: input.mcp.url, headers: { Authorization: `Bearer ${process.env.OPENTRADER_MCP_TOKEN ?? ""}` }, alwaysLoad: true },
      },
      // Every call goes through here: the app's tools and the web are allowed, nothing else.
      canUseTool: async (name, args) => (allowed(name) ? { behavior: "allow", updatedInput: args } : { behavior: "deny", message: "Not available in OpenTrader." }),
      settingSources: [],
      persistSession: false,
      includePartialMessages: true,
      abortController,
      cwd: workDir(),
      env: { ...process.env, CLAUDE_AGENT_SDK_CLIENT_APP: "opentrader-assistant/1" },
    },
  });
  let wrote = false;
  for await (const m of q) {
    if (m.type === "stream_event" && m.parent_tool_use_id == null) {
      const e = m.event;
      if (e.type === "content_block_start" && e.content_block?.type === "text" && wrote) emit({ t: "text", text: "\n\n" });
      if (e.type === "content_block_delta" && e.delta?.type === "text_delta" && e.delta.text) {
        emit({ t: "text", text: e.delta.text });
        wrote = true;
      }
    } else if (m.type === "assistant" && m.parent_tool_use_id == null) {
      for (const b of m.message?.content ?? []) {
        if (b.type === "tool_use") emit({ t: "tool", id: b.id, name: shortName(b.name), args: JSON.stringify(b.input ?? {}) });
      }
    } else if (m.type === "user" && Array.isArray(m.message?.content)) {
      for (const b of m.message.content) {
        if (b.type === "tool_result") emit({ t: "tool_done", id: b.tool_use_id, ok: !b.is_error, result: cut(textOf(b.content)) });
      }
    } else if (m.type === "result") {
      if (m.subtype !== "success" || m.is_error) {
        const why = (m.errors && m.errors.length ? m.errors.join("; ") : m.result) || m.subtype;
        throw new Error(`Claude Code: ${why}`);
      }
      if (!wrote && m.result) emit({ t: "text", text: m.result });
    }
  }
}

// ── Codex (Codex SDK) ────────────────────────────────────────────────────
/**
 * Codex's own home for the assistant: the user's ~/.codex would bring their MCP servers, plugins and skills (a
 * Node REPL, for one, that runs any command). It holds only their login (a link to ~/.codex/auth.json, so a
 * refreshed login is kept).
 */
export function codexHome(base = os.tmpdir(), userHome = process.env.CODEX_HOME || path.join(os.homedir(), ".codex")) {
  const dir = path.join(base, "opentrader-assistant-codex");
  fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
  const auth = path.join(dir, "auth.json");
  const source = path.join(userHome, "auth.json");
  fs.rmSync(auth, { force: true });
  if (fs.existsSync(source)) fs.symlinkSync(source, auth);
  return dir;
}

/** The Codex items the assistant may run: the app's tools and web search; anything else stops the answer. */
export function codexItemAllowed(item) {
  if (!item?.type) return true;
  if (item.type === "mcp_tool_call") return item.server === SERVER;
  return !["command_execution", "file_change"].includes(item.type);
}

async function runCodex(input, signal) {
  const { Codex } = await import("@openai/codex-sdk");
  const env = Object.fromEntries(Object.entries(process.env).filter(([, v]) => v !== undefined));
  const codex = new Codex({
    ...(process.env.CODEX_API_KEY ? { apiKey: process.env.CODEX_API_KEY } : {}),
    env: { ...env, CODEX_HOME: codexHome() },
    config: {
      developer_instructions: input.system,
      // No shell, file editing or image reading: only the app's tools (and web search) (REQ-ASSIST-CLI-003).
      features: {
        shell_tool: false, unified_exec: false, view_image: false, image_generation: false, apps: false, plugins: false,
        hooks: false, browser_use: false, computer_use: false, multi_agent: false, skill_search: false, tool_suggest: false,
      },
      mcp_servers: {
        // No approval prompt (nobody could answer it): the app itself asks the user before a destructive tool.
        [SERVER]: { url: input.mcp.url, bearer_token_env_var: "OPENTRADER_MCP_TOKEN", default_tools_approval_mode: "approve" },
      },
    },
  });
  const thread = codex.startThread({
    ...(input.model ? { model: input.model } : {}),
    // No shell (features above); the sandbox stays read-only, in an empty folder, should one be added back.
    sandboxMode: "read-only",
    approvalPolicy: "never",
    workingDirectory: workDir(),
    skipGitRepoCheck: true,
    networkAccessEnabled: false,
    webSearchMode: input.web ? "live" : "disabled",
  });
  const { events } = await thread.runStreamed(input.prompt, { signal });
  let wrote = false;
  for await (const e of events) {
    const item = e.item;
    if (e.type.startsWith("item.") && !codexItemAllowed(item)) {
      throw new Error(`Codex: it tried to use ${item.type === "mcp_tool_call" ? `${item.server}/${item.tool}` : item.type}, which the assistant does not allow`);
    }
    if (e.type === "item.started" && item?.type === "mcp_tool_call") {
      emit({ t: "tool", id: item.id, name: item.tool, args: JSON.stringify(item.arguments ?? {}) });
    } else if (e.type === "item.completed" && item?.type === "mcp_tool_call") {
      const ok = item.status === "completed" && !item.error;
      emit({ t: "tool_done", id: item.id, ok, result: cut(ok ? textOf(item.result?.content) : item.error?.message ?? "failed") });
    } else if (e.type === "item.completed" && item?.type === "agent_message" && item.text) {
      emit({ t: "text", text: wrote ? `\n\n${item.text}` : item.text });
      wrote = true;
    } else if (e.type === "turn.failed") {
      throw new Error(`Codex: ${e.error?.message ?? "the turn failed"}`);
    } else if (e.type === "error") {
      throw new Error(`Codex: ${e.message}`);
    }
  }
}

// ── Gemini (Google Gen AI SDK, the app's tools over an MCP client) ───────
/** The longest tool loop: past it the answer stops. */
const GEMINI_ROUNDS = 24;

async function runGemini(input, signal) {
  const key = process.env.GEMINI_API_KEY || process.env.GOOGLE_API_KEY;
  if (!key) throw new Error("Gemini: set GEMINI_API_KEY in Settings > Coding agents");
  const { GoogleGenAI } = await import("@google/genai");
  const { Client } = await import("@modelcontextprotocol/sdk/client/index.js");
  const { StreamableHTTPClientTransport } = await import("@modelcontextprotocol/sdk/client/streamableHttp.js");
  const mcp = new Client({ name: "opentrader-assistant", version: "1.0.0" });
  await mcp.connect(
    new StreamableHTTPClientTransport(new URL(input.mcp.url), { requestInit: { headers: { Authorization: `Bearer ${process.env.OPENTRADER_MCP_TOKEN ?? ""}` } } }),
  );
  try {
    const { tools } = await mcp.listTools();
    const functionDeclarations = tools.map((t) => ({ name: t.name, description: t.description ?? t.title ?? t.name, parametersJsonSchema: t.inputSchema }));
    const ai = new GoogleGenAI({ apiKey: key });
    const contents = [{ role: "user", parts: [{ text: input.prompt }] }];
    const config = {
      systemInstruction: input.system,
      tools: [{ functionDeclarations }, ...(input.web ? [{ googleSearch: {} }] : [])],
      abortSignal: signal,
    };
    let wrote = false;
    let n = 0;
    for (let round = 0; round < GEMINI_ROUNDS; round++) {
      const stream = await ai.models.generateContentStream({ model: input.model || "gemini-pro-latest", contents, config });
      const parts = [];
      const calls = [];
      let roundWrote = false;
      for await (const chunk of stream) {
        for (const p of chunk.candidates?.[0]?.content?.parts ?? []) {
          parts.push(p);
          if (p.functionCall) calls.push(p.functionCall);
          else if (p.text && !p.thought) {
            emit({ t: "text", text: wrote && !roundWrote ? `\n\n${p.text}` : p.text });
            wrote = roundWrote = true;
          }
        }
      }
      // The model's turn as it came (its thought signatures included), then the results of its calls.
      contents.push({ role: "model", parts });
      if (!calls.length) return;
      const responses = [];
      for (const call of calls) {
        const id = call.id ?? `gemini${++n}`;
        emit({ t: "tool", id, name: call.name, args: JSON.stringify(call.args ?? {}) });
        let ok = true;
        let text;
        try {
          const r = await mcp.callTool({ name: call.name, arguments: call.args ?? {} });
          ok = !r.isError;
          text = textOf(r.content);
        } catch (e) {
          ok = false;
          text = errorText(e);
        }
        emit({ t: "tool_done", id, ok, result: cut(text) });
        responses.push({ functionResponse: { ...(call.id ? { id: call.id } : {}), name: call.name, response: ok ? { output: text } : { error: text } } });
      }
      contents.push({ role: "user", parts: responses });
    }
    throw new Error("Gemini: stopped after too many steps; ask again to go on");
  } finally {
    await mcp.close().catch(() => {});
  }
}

const RUNNERS = { claude: runClaude, codex: runCodex, gemini: runGemini };

async function readStdin() {
  let s = "";
  for await (const chunk of process.stdin) s += chunk;
  return JSON.parse(s);
}

async function main() {
  const abort = new AbortController();
  const quit = () => {
    abort.abort();
    setTimeout(() => process.exit(130), 1500).unref();
  };
  process.on("SIGTERM", quit);
  process.on("SIGINT", quit);
  let input;
  try {
    input = await readStdin();
  } catch (e) {
    emit({ t: "error", message: `Bad request: ${errorText(e)}` });
    process.exit(2);
  }
  const run = RUNNERS[input.provider];
  if (!run) {
    emit({ t: "error", message: `Unknown model ${input.provider}` });
    process.exit(2);
  }
  try {
    await run(input, abort.signal);
    emit({ t: "done" });
    process.exit(0);
  } catch (e) {
    const why = errorText(e);
    emit({ t: "error", message: abort.signal.aborted ? "Stopped." : input.provider === "gemini" && !why.startsWith("Gemini") ? `Gemini: ${why}` : why });
    process.exit(1);
  }
}

if (process.argv[1] && fs.realpathSync(path.resolve(process.argv[1])) === fileURLToPath(import.meta.url)) void main();
