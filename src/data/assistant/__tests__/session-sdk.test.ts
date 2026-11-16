/* The assistant's SDK models (specs/assistant-cli-models.md): the prompt they get, their events in the answer, the
 * confirmation of their destructive tools, and no statistics. */
import { describe, expect, it, vi } from "vitest";

vi.mock("../../kv", () => ({
  getItem: (k: string) => (k === "ot:assistant:model" ? "claude" : null),
  setItem: () => undefined,
  removeItem: () => undefined,
  onExternalChange: () => () => undefined,
}));
vi.mock("@tauri-apps/api/core", () => ({
  Channel: class {
    onmessage: (d: string) => void = () => {};
  },
}));
const sent = vi.hoisted(() => ({ events: [] as string[] }));
vi.mock("../../analytics", () => ({ track: (event: string) => sent.events.push(event) }));
// The SDK run: the events queued in `script` (a function may stand for an event: it runs at that point).
const run = vi.hoisted(() => ({ script: [] as (object | (() => Promise<void>))[][], asked: [] as any[] }));
vi.mock("../../../bindings", () => ({
  commands: {
    assistantChat: async () => ({ status: "error", error: "the built-in model was asked" }),
    assistantCancel: async () => undefined,
    assistantCliCancel: async () => undefined,
    assistantCliStatus: async () => ({ node: "/usr/bin/node", installed: true, models: [] }),
    assistantCliChat: async (_req: number, id: string, body: string, ch: { onmessage: (d: string) => void }) => {
      run.asked.push({ id, ...JSON.parse(body) });
      for (const e of run.script.shift() ?? []) {
        if (typeof e === "function") await e();
        else ch.onmessage(JSON.stringify(e));
      }
      return { status: "ok", data: null };
    },
  },
}));

import { activeChat, answerConfirm, newChat, sdkPrompt, send, setAssistantHost, type Chat } from "../session";
import { assistantConfirm } from "../../../agent/assistant-confirm";
import { SDK_INSTRUCTIONS } from "../prompts";

setAssistantHost({
  name: "opentrader",
  version: () => "1",
  instructions: "",
  tools: [{ name: "get_chart", title: "", description: "", inputSchema: { type: "object" }, run: async () => ({ panes: [{ symbol: "NSE:RELIANCE", interval: "1D" }] }) }],
  resources: () => [],
  prompts: [],
});

const last = () => {
  const ms = activeChat()!.messages;
  return ms[ms.length - 1] as Extract<Chat["messages"][number], { role: "assistant" }>;
};

describe("the assistant's SDK models", () => {
  it("get today's date, the chart on screen and the chat so far (REQ-ASSIST-CLI-002.A1)", () => {
    const c: Chat = {
      id: "c",
      title: "",
      createdAt: 0,
      updatedAt: 0,
      messages: [
        { role: "user", text: "Hi" },
        { role: "assistant", text: "Hello", tools: [{ id: "1", name: "get_bars", arguments: "{}", status: "done" }] },
        { role: "user", text: "what is the trend of this chart?" },
      ],
    };
    const p = sdkPrompt(c, '{"symbol":"NSE:RELIANCE","interval":"1D"}', new Date(2026, 10, 16));
    expect(p).toContain("Today is 16/11/2026.");
    expect(p).toContain("NSE:RELIANCE");
    expect(p).toContain("1D");
    expect(p).toContain("User: Hi");
    expect(p).toContain("[get_bars done]");
    expect(p.endsWith("The user's message:\nwhat is the trend of this chart?")).toBe(true);
  });

  it("are told to rest on the app's data (REQ-ASSIST-CLI-004.A1)", () => {
    for (const rule of ["Data first", "last bar", "implied volatility", "Greeks", "entry", "invalidation", "risks", "cannot place"]) expect(SDK_INSTRUCTIONS).toContain(rule);
  });

  it("stream their answer and steps into the chat, with no statistics (REQ-ASSIST-CLI-002, 005.A1)", async () => {
    newChat();
    sent.events.length = 0;
    run.script.push([
      { t: "status", text: "Setting up…" },
      { t: "tool", id: "t1", name: "get_bars", args: '{"symbol":"NSE:INFY"}' },
      { t: "tool_done", id: "t1", ok: true, result: "[[1,2,3,4,5]]" },
      { t: "text", text: "RSI is " },
      { t: "text", text: "61." },
      { t: "done" },
    ]);
    await send("RSI of NSE:INFY on 1D?");
    const m = last();
    expect(run.asked[run.asked.length - 1].id).toBe("claude");
    expect(run.asked[run.asked.length - 1].system).toBe(SDK_INSTRUCTIONS);
    expect(run.asked[run.asked.length - 1].prompt).toContain("NSE:RELIANCE");
    expect(m.text).toBe("RSI is 61.");
    expect(m.model).toBe("claude");
    expect(m.status).toBeUndefined();
    expect(m.tools).toEqual([{ id: "t1", name: "get_bars", arguments: '{"symbol":"NSE:INFY"}', status: "done", result: "[[1,2,3,4,5]]" }]);
    expect(m.trace).toBeUndefined();
    expect(sent.events).toEqual([]);
  });

  it("show the run's error, and do not ask the built-in model (REQ-ASSIST-CLI-002.A3)", async () => {
    newChat();
    run.script.push([{ t: "error", message: "Claude Code: Invalid API key" }]);
    await send("hello");
    expect(last().error).toBe("Claude Code: Invalid API key");
  });

  it("wait for Allow before a destructive tool; Don't allow tells the model (REQ-ASSIST-CLI-003.A3)", async () => {
    newChat();
    let allowed: boolean | undefined;
    run.script.push([
      { t: "tool", id: "d1", name: "delete_alert", args: '{"id":"a1"}' },
      async () => {
        const answer = assistantConfirm("delete_alert", { id: "a1" });
        await Promise.resolve();
        expect(last().tools![0].status).toBe("confirm");
        answerConfirm("d1", false);
        allowed = await answer;
      },
      { t: "tool_done", id: "d1", ok: false, result: "The user did not allow it." },
      { t: "text", text: "Left the alert." },
      { t: "done" },
    ]);
    await send("delete my alert");
    expect(allowed).toBe(false);
    expect(last().tools![0]).toMatchObject({ status: "denied", result: "The user did not allow it." });
  });
});
