import { describe, expect, it, vi } from "vitest";

const listeners: (() => void)[] = [];
let served = [
  { key: "moomoo:US", plugin: "moomoo", pluginName: "moomoo", market: "US" },
  { key: "zerodha:IN", plugin: "zerodha", pluginName: "Zerodha", market: "IN" },
];
vi.mock("../../../bindings", () => ({
  commands: { pluginScreenerMarkets: () => Promise.resolve(served) },
  events: { pluginsChanged: { listen: (cb: () => void) => (listeners.push(cb), Promise.resolve(() => {})) } },
}));

const m = await import("../plugin-markets");
const q = await import("../../../data/screener-query");
const tick = () => new Promise((r) => setTimeout(r, 0));

describe("plugin screener markets", () => {
  /** REQ-SCREEN-MKT-001.A1: USA, India, USA (moomoo), in that order. */
  it("lists USA, then India, then USA (moomoo)", async () => {
    m.pluginMarkets();
    await tick();
    expect([m.USA, ...m.pluginMarkets().map(m.viewOf)].map((v) => v.title)).toEqual(["USA", "India", "USA (moomoo)"]);
    expect(m.activeView({ market: "zerodha:IN" })).toMatchObject({ title: "India", flag: "scr-flag-in" });
  });

  /** REQ-SCREEN-MKT-001.A3: a saved market whose plugin is off shows USA, with the reason. */
  it("falls back to USA when the plugin is no longer enabled", async () => {
    served = served.filter((x) => x.plugin !== "zerodha");
    listeners.forEach((l) => l());
    await tick();
    expect(m.activeMarket({ market: "zerodha:IN" })).toBeNull();
    expect(m.activeView({ market: "zerodha:IN" }).title).toBe("USA");
    expect(m.missingMarketNotice({ market: "zerodha:IN" })).toContain("needs the Zerodha plugin");
    expect(m.missingMarketNotice({})).toBeNull();
  });

  /** REQ-SCREEN-MKT-003.A1: Primary listing keeps one row per company in a plugin market. */
  it("scans a plugin market's stocks, and its second listings with Primary listing off", () => {
    expect(q.universeClause({ market: "zerodha:IN" }).right).toEqual(["stock"]);
    expect(q.universeClause({ market: "zerodha:IN", primaryListing: false }).right).toEqual(["stock", "secondary"]);
    expect(q.universeClause({}).right).toEqual(q.PRIMARY_UNIVERSE_CLAUSE.right);
  });

  it("writes the rows' time as dd/mm/yyyy", () => {
    expect(m.formatDateTime(new Date(2025, 10, 13, 15, 29, 59).getTime())).toBe("13/11/2025 15:29:59");
  });
});
