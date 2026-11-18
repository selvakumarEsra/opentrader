import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { codexHome, codexItemAllowed, errorText, shortName, textOf } from "./bridge.mjs";

test("tool names lose the MCP server's prefix", () => {
  assert.equal(shortName("mcp__opentrader__get_bars"), "get_bars");
  assert.equal(shortName("WebSearch"), "WebSearch");
});

test("content blocks read as text", () => {
  assert.equal(textOf([{ type: "text", text: "a" }, { type: "image", data: "x", mimeType: "image/png" }]), "a\n[picture]");
  assert.equal(textOf("plain"), "plain");
  assert.equal(textOf(undefined), "");
});

test("API errors given as JSON read as their message", () => {
  const nested = JSON.stringify({ error: { message: JSON.stringify({ error: { code: 400, message: "API key not valid." } }) } });
  assert.equal(errorText(new Error(nested)), "API key not valid.");
  assert.equal(errorText(new Error("plain")), "plain");
});

test("Codex may use only the app's tools", () => {
  assert.ok(codexItemAllowed({ type: "mcp_tool_call", server: "opentrader", tool: "get_bars" }));
  assert.ok(codexItemAllowed({ type: "agent_message", text: "hi" }));
  assert.ok(codexItemAllowed({ type: "web_search", query: "x" }));
  assert.ok(!codexItemAllowed({ type: "mcp_tool_call", server: "node_repl", tool: "js" }));
  assert.ok(!codexItemAllowed({ type: "command_execution", command: "ls ~" }));
  assert.ok(!codexItemAllowed({ type: "file_change", changes: [] }));
});

test("Codex's home holds only the user's login", () => {
  const base = fs.mkdtempSync(path.join(os.tmpdir(), "ot-codex-"));
  const user = path.join(base, "user");
  fs.mkdirSync(user);
  fs.writeFileSync(path.join(user, "auth.json"), "{}");
  fs.writeFileSync(path.join(user, "config.toml"), "[mcp_servers.x]");
  const home = codexHome(base, user);
  assert.deepEqual(fs.readdirSync(home), ["auth.json"]);
  assert.equal(fs.realpathSync(path.join(home, "auth.json")), fs.realpathSync(path.join(user, "auth.json")));
  fs.rmSync(path.join(user, "auth.json"));
  assert.deepEqual(fs.readdirSync(codexHome(base, user)), []);
  fs.rmSync(base, { recursive: true });
});
