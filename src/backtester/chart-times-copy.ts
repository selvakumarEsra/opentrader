/*
 * A copy of a live result on the chart's times (specs/performance.md REQ-PERF-002). The study worker sends the
 * chart's times and keeps its run, and the result it compares the next one with, on the script's times: a live
 * update re-stamps the points it sends only. Kept apart from oakscript.ts.
 */
import { isPackedSeries } from 'lightweight-charts-indicators/helpers';
import { restoreTimes } from './oakscript';
import type { Bar } from './types';

/** restoreTimes on a copy of `v` (a study's result or the changes of one) that keeps its packed lists packed and the
 *  objects it holds twice held twice. */
export function chartTimesCopy<T>(v: T, seen: Bar[], bars: Bar[]): T {
  const copies = new Map<object, unknown>();
  const copy = (node: unknown): unknown => {
    if (!node || typeof node !== 'object') return node;
    const done = copies.get(node);
    if (done) return done;
    if (Array.isArray(node)) {
      // A packed list's copy is packed (its points are held as values).
      if (isPackedSeries(node)) {
        const out = node.slice();
        copies.set(node, out);
        return out;
      }
      const out: unknown[] = [];
      copies.set(node, out);
      for (const item of node) out.push(copy(item));
      return out;
    }
    if (Object.getPrototypeOf(node) !== Object.prototype) return node;
    const out: Record<string, unknown> = {};
    copies.set(node, out);
    for (const k in node as Record<string, unknown>) out[k] = copy((node as Record<string, unknown>)[k]);
    return out;
  };
  const out = copy(v) as T;
  restoreTimes(out, seen, bars);
  return out;
}
