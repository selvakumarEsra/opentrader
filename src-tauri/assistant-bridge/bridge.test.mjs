import { test } from "node:test";
import assert from "node:assert/strict";
import { errorText, shortName, textOf } from "./bridge.mjs";

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
