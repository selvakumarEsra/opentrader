/*
 * A live strategy's saved state and its report as sent (specs/performance.md REQ-PERF-002).
 *
 * A live strategy restores the broker state of its last closed bar on every tick. Its lists that only grow (closed
 * trades, fills) keep their items, which are not changed once added: the state copies the lists only, not their
 * items (a deep copy of a long backtest's state took about 18 ms per tick). Kept apart from oakscript.ts, whose
 * BrokerEngine.save / restore call it.
 */
import type { Bar } from './types';
import type { DeltaBound } from '../window/chart/indicators/delta-encodings';

/** Broker lists that only grow, their items not changed once added (closed trades, fills): a state keeps their items
 *  and copies the list only (REQ-PERF-002.A5). */
const BROKER_APPENDED = new Set(['closedTrades', 'filledOrders']);
/** Broker lists that only grow but change their last item in place (a margin point of the same time). */
const BROKER_LAST_CHANGES = new Set(['marginUsage']);

const listCopy = (key: string, list: unknown[]): unknown[] => {
  const out = list.slice();
  const last = out.length - 1;
  if (BROKER_LAST_CHANGES.has(key) && last >= 0) out[last] = { ...(out[last] as object) };
  return out;
};

/** What BrokerEngine.save() gives: the broker state (orders, trades, position, report records), its growing lists
 *  apart, and the equity. */
type EngineState = { broker: Record<string, unknown>; lists: Record<string, unknown[]>; equity: number[] };

/** The state of `broker` (less its `setup` fields) and `equity`. */
export function saveEngineState(broker: object, setup: ReadonlySet<string>, equity: number[]): unknown {
  const fields: Record<string, unknown> = {};
  const lists: Record<string, unknown[]> = {};
  for (const [k, v] of Object.entries(broker)) {
    if (setup.has(k)) continue;
    if ((BROKER_APPENDED.has(k) || BROKER_LAST_CHANGES.has(k)) && Array.isArray(v)) lists[k] = listCopy(k, v);
    else fields[k] = v;
  }
  return { broker: structuredClone(fields), lists, equity: equity.slice() } satisfies EngineState;
}

/** Puts a state of saveEngineState back on `broker` and `equity`, copied again (it is restored on each update until
 *  the next bar closes): the growing lists as lists, their items kept. */
export function restoreEngineState(state: unknown, broker: object, equity: number[]): void {
  const saved = state as EngineState;
  const s = structuredClone(saved.broker);
  for (const [k, list] of Object.entries(saved.lists)) s[k] = listCopy(k, list);
  Object.assign(broker, s);
  const n = Math.min(saved.equity.length, equity.length);
  for (let i = 0; i < n; i++) equity[i] = saved.equity[i];
}

/** `v` with its objects and lists copied (`depth` levels down), the items of the lists kept: what a later encodeDelta
 *  compares with, apart from the lists the broker goes on changing (it adds to its filled orders). The items are not
 *  changed once made (closed trades, fills; the open trades are new reports on every run), so the next report's are
 *  found the same by identity. */
export function listsCopied<T>(v: T, depth = 3): T {
  if (depth < 0 || v === null || typeof v !== 'object') return v;
  if (Array.isArray(v)) return v.slice() as T;
  if (Object.getPrototypeOf(v) !== Object.prototype) return v;
  const out: Record<string, unknown> = {};
  for (const k in v) out[k] = listsCopied((v as Record<string, unknown>)[k], depth - 1);
  return out as T;
}

/** A resumed run's bound: what it drew before its first recalculated bar `from` is what it drew before (its output
 *  values are new points every update, compared from that bar's time). None for a run of every bar. */
export function resumedBound(bars: Bar[], from: number | undefined): DeltaBound | undefined {
  const f = from ?? 0;
  return f > 0 && f < bars.length ? { from: f, bars: bars.length, fromTime: bars[f].time } : undefined;
}
