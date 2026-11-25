/* specs/performance.md REQ-PERF-002: a live update of a long chart sends the points that changed, and the result the
 * window rebuilds from them equals a full calculation (the real indicator library, intraday and restamped daily). */
import { describe, expect, it } from "vitest";
import type { Bar } from "oakscriptjs";

const { calculateStudy } = await import("../study-calc");
const { applyDelta, jobChanges } = await import("../study-worker-types");
const { fromPost } = await import("../packed-points");
const { restoreTimes, scriptBars } = await import("../../../../backtester/oakscript");
const { loadIndicator } = await import("lightweight-charts-indicators/loader");

/** A full calculation on the chart's times, as the window made it before the worker (calculate, then restoreTimes). */
async function fullOnChart(id: string, bars: Bar[], ctx: object): Promise<unknown> {
  const entry = (await loadIndicator(id)) as unknown as { calculate: (b: Bar[], i: object, c: object) => unknown };
  const seen = scriptBars(bars.map((b) => ({ ...b })), ctx as never);
  const result = entry.calculate(seen, {}, ctx);
  restoreTimes(result, seen, bars);
  return result;
}

let ids = 0;
/** The window's side of the protocol (as IndicatorLayer.runOffThread), with the size of each reply. */
function client(entryId: string, ctx: object, restamp: boolean) {
  const sent = { stateId: null as number | null, bars: null as Bar[] | null, data: new Map<string, unknown>() };
  const study = `chart|${entryId}|${++ids}`;
  let version: number | null = null;
  let raw: unknown = null;
  return {
    async update(bars: Bar[]): Promise<{ result: unknown; bytes: number }> {
      for (;;) {
        const changes = jobChanges(sent, bars, {}, (v) => JSON.stringify(v));
        const job = structuredClone({ kind: "study" as const, id: ++ids * 1000, entryId, study, base: version, restamp, inputs: {}, ctx, accessors: [], sources: {}, ...changes });
        const reply = structuredClone(await calculateStudy(job as never)) as { id: number; base?: number; retry?: boolean; result: unknown };
        if (reply.retry) {
          sent.stateId = null;
          version = null;
          raw = null;
          continue;
        }
        const bytes = JSON.stringify(reply.result ?? null).length;
        reply.result = fromPost(reply.result);
        sent.stateId = reply.id;
        if (reply.base != null) reply.result = applyDelta(reply.result, raw);
        version = reply.id;
        raw = reply.result;
        return { result: raw, bytes };
      }
    },
  };
}

function walk(n: number, step: number, t0: number): Bar[] {
  let p = 100;
  let s = 7;
  const r = () => (s = (s * 1103515245 + 12345) & 0x7fffffff) / 0x7fffffff - 0.5;
  return Array.from({ length: n }, (_, i) => {
    const o = p;
    p = Math.max(1, p + r());
    return { time: t0 + i * step, open: o, high: Math.max(o, p) + 0.2, low: Math.min(o, p) - 0.2, close: p, volume: 1000 + i };
  });
}

/** A result with its plots that have no value as empty lists (as the worker sends them). */
function withoutEmpty(result: unknown): unknown {
  const r = result as { plots?: Record<string, { value?: number }[]> };
  if (!r.plots) return result;
  const plots = Object.fromEntries(Object.entries(r.plots).map(([k, l]) => [k, l.some((p) => Number.isFinite(p?.value)) ? l : []]));
  return { ...r, plots };
}

/** A result as plain values (packed lists read as arrays). */
const plain = (v: unknown) => JSON.parse(JSON.stringify(v));

const INTRADAY = { timeframe: "1", timezone: "America/New_York", session: "0930-1600" };
const DAILY = { timeframe: "1D", timezone: "America/New_York", session: "0930-1600" };
// Daily bars at New York midnight (the datafeed's stamps): the worker restamps them to the session open.
const MIDNIGHT = Date.UTC(2010, 0, 4, 5) / 1000;

describe("live study updates on long charts", () => {
  for (const [label, ctx, restamp, step, t0] of [
    ["intraday", INTRADAY, false, 60, 1_600_000_000],
    ["daily, restamped", DAILY, true, 86_400, MIDNIGHT],
  ] as const) {
    for (const id of ["rsi", "ichimoku", "macd", "vwap"]) {
      it(`${id} ${label}: ticks and a new bar give a full calculation's result, in a few kilobytes`, async () => {
        const bars = walk(3000, step, t0);
        const live = client(id, ctx, restamp);
        await live.update(bars);
        let biggest = 0;
        for (let t = 0; t < 6; t++) {
          const last = bars[bars.length - 1]!;
          if (t === 3) bars.push({ ...last, time: last.time + step, open: last.close, close: last.close + 0.3 });
          else bars[bars.length - 1] = { ...last, close: last.close + 0.05 * (t - 2), high: last.high + 0.05 };
          biggest = Math.max(biggest, (await live.update(bars)).bytes);
        }
        const full = await client(id, ctx, restamp).update(bars);
        const result = plain((await live.update(bars)).result);
        expect(result).toEqual(plain(full.result));
        expect(result).toEqual(plain(withoutEmpty(await fullOnChart(id, bars, ctx))));
        // The changes of a tick, not the result (the full one is hundreds of kilobytes).
        expect(biggest).toBeLessThan(Math.min(20_000, full.bytes / 10));
      });
    }
  }
});
