/* The window holds the library's entries without their code (specs/performance.md REQ-PERF-001): the study worker
 * calculates them; the window imports the code only to calculate on its own thread. */
import { afterAll, beforeAll, describe, expect, it, vi } from "vitest";
import meta from "virtual:indicator-meta";

/** Whether `v` is a function or holds one, at any depth (up to 12 levels). */
function holdsFunction(v: unknown, depth = 0): boolean {
  return typeof v === "function" || (depth < 12 && v !== null && typeof v === "object" && Object.values(v).some((x) => holdsFunction(x, depth + 1)));
}

describe("indicator registry with workers: the library's metadata, its code on demand", () => {
  beforeAll(() => vi.stubGlobal("Worker", class {}));
  afterAll(() => vi.unstubAllGlobals());

  it("has every entry's metadata and no function in it (REQ-PERF-001.A1)", async () => {
    const { indicatorRegistry } = await import("lightweight-charts-indicators");
    expect(meta.entries.map((e) => e.id)).toEqual(indicatorRegistry.map((e) => e.id));
    expect(meta.entries.some((e) => holdsFunction(e))).toBe(false);
    const rsi = indicatorRegistry.find((e) => e.id === "rsi")!;
    const { calculate: _c, createRun: _r, ...data } = rsi as unknown as Record<string, unknown>;
    expect(meta.entries.find((e) => e.id === "rsi")).toEqual(JSON.parse(JSON.stringify(data)));
  });

  it("gives entries without code, then puts the code on the same entries (REQ-PERF-001.A2, A3)", async () => {
    vi.resetModules();
    const reg = await import("../registry");
    await reg.loadIndicatorLibrary();
    const rsi = reg.getIndicatorEntry("rsi")!;
    expect(rsi.plotConfig.length).toBeGreaterThan(0);
    expect(rsi.calculate).toBeUndefined();
    expect(reg.indicatorCodeLoaded()).toBe(false);
    // A calculation on this thread asks for the code.
    expect(reg.canCalculateHere(rsi)).toBe(false);
    await reg.loadIndicatorCode();
    expect(reg.getIndicatorEntry("rsi")).toBe(rsi);
    expect(typeof rsi.calculate).toBe("function");
    expect(reg.canCalculateHere(rsi)).toBe(true);
  });
});
