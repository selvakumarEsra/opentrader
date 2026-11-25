/* The assistant's model choice (specs/assistant-cli-models.md, REQ-ASSIST-CLI-001): an SDK model by default, the
 * built-in one only when chosen, the fallback only when turned on. */
import { beforeEach, describe, expect, it, vi } from "vitest";

const store = vi.hoisted(() => new Map<string, string>());
vi.mock("../../kv", () => ({
  getItem: (k: string) => store.get(k) ?? null,
  setItem: (k: string, v: string) => void store.set(k, v),
  removeItem: (k: string) => void store.delete(k),
  onExternalChange: () => () => undefined,
}));
vi.mock("../../../bindings", () => ({ commands: {} }));

type Ready = Partial<Record<"claude" | "codex" | "gemini", boolean>>;
const statusOf = (ready: Ready) => ({
  node: "/usr/bin/node",
  installed: true,
  models: (["claude", "codex", "gemini"] as const).map((id) => ({
    id,
    name: { claude: "Claude Code", codex: "Codex", gemini: "Gemini" }[id],
    ready: !!ready[id],
    need: ready[id] ? null : "a token",
    source: ready[id] ? "your login" : null,
    tokenSet: false,
    model: "",
  })),
});

async function load(saved: Record<string, string> = {}) {
  store.clear();
  for (const [k, v] of Object.entries(saved)) store.set(k, v);
  vi.resetModules();
  return import("../models");
}

describe("the assistant's model", () => {
  beforeEach(() => store.clear());

  it("is the first ready SDK model until the user chooses (REQ-ASSIST-CLI-001.A1)", async () => {
    const m = await load();
    m.setModelsStatus(statusOf({ claude: true, codex: true }) as never);
    expect(m.activeModel()).toBe("claude");
    m.setModelsStatus(statusOf({ codex: true }) as never);
    expect(m.activeModel()).toBe("codex");
    expect(m.hasChosen()).toBe(false);
  });

  it("is never the built-in one by default, even with no SDK model ready (REQ-ASSIST-CLI-001.A3)", async () => {
    const m = await load();
    m.setModelsStatus(statusOf({}) as never);
    expect(m.activeModel()).toBe("claude");
    expect(m.notReady()?.id).toBe("claude");
    expect(m.fellBack()).toBeUndefined();
  });

  it("is the chosen one, kept (REQ-ASSIST-CLI-001.A2)", async () => {
    let m = await load();
    m.setModelsStatus(statusOf({ claude: true, codex: true }) as never);
    m.chooseModel("codex");
    expect(m.activeModel()).toBe("codex");
    m = await load(Object.fromEntries(store));
    m.setModelsStatus(statusOf({ claude: true, codex: true }) as never);
    expect(m.activeModel()).toBe("codex");
  });

  it("falls back to the built-in one only when that is turned on (REQ-ASSIST-CLI-001.A3)", async () => {
    const m = await load({ [`ot:assistant:model`]: "codex" });
    m.setModelsStatus(statusOf({ claude: true }) as never);
    expect(m.activeModel()).toBe("codex");
    expect(m.notReady()?.id).toBe("codex");
    m.setBuiltinFallback(true);
    expect(m.activeModel()).toBe(m.BUILTIN);
    expect(m.fellBack()?.id).toBe("codex");
    expect(m.notReady()).toBeUndefined();
  });

  it("is the built-in one when chosen (REQ-ASSIST-CLI-001.A4)", async () => {
    const m = await load({ [`ot:assistant:model`]: "builtin" });
    m.setModelsStatus(statusOf({ claude: true }) as never);
    expect(m.activeModel()).toBe(m.BUILTIN);
    expect(m.notReady()).toBeUndefined();
  });
});
