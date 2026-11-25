/* The title bar's data badge (specs/data-source-badge.md REQ-DATA-BADGE-001). */
import { describe, expect, it, vi } from "vitest";

const served = vi.hoisted(() => new Map<string, string[]>());
vi.mock("../../../data/session/symbol-sessions", () => ({
  cachedSymbolSessions: (s: string) => (served.has(s) ? { intervals: served.get(s) } : null),
}));

import { dataBadge, servedByPlugin } from "../data-source-badge";

const base = { plugin: false, paid: false, realtimeActive: false, realtimeElsewhere: false };

describe("the data badge", () => {
  it("names a data plugin as the source, with no delay of the app's own (A1, A2)", () => {
    expect(dataBadge({ ...base, plugin: true })).toEqual({ label: "Plugin data", tip: "This chart's data comes from a data plugin; any delay is the plugin's.", realtime: false });
    const paid = dataBadge({ ...base, plugin: true, paid: true, realtimeActive: true });
    expect(paid.label).toBe("Plugin data");
    expect(paid.realtime).toBe(false);
    expect(paid.tip).toMatch(/Click/);
  });

  it("is as before for the app's own data (A3)", () => {
    expect(dataBadge(base).label).toBe("Delayed 15 min");
    expect(dataBadge({ ...base, paid: true }).tip).toBe("Market data is delayed by 15 minutes. Click for real-time data.");
    expect(dataBadge({ ...base, paid: true, realtimeActive: true })).toMatchObject({ label: "Real-time", realtime: true });
    expect(dataBadge({ ...base, paid: true, realtimeElsewhere: true }).label).toBe("Real-time elsewhere");
  });

  it("takes a symbol as a plugin's once its sessions list served intervals (A3)", () => {
    served.set("HK:00700", ["1", "D"]);
    served.set("NASDAQ:AAPL", []);
    expect(servedByPlugin("HK:00700")).toBe(true);
    expect(servedByPlugin("NASDAQ:AAPL")).toBe(false);
    expect(servedByPlugin("NYSE:IBM")).toBe(false); // not resolved yet
    expect(servedByPlugin(null)).toBe(false);
  });
});
