/*
 * The delta encodings of a live study or strategy result beyond a list's tail (specs/performance.md REQ-PERF-002),
 * used by study-worker-types' encodeDelta / applyDelta / advanceSnapshot:
 *
 * - ITEMS: a short list of objects holding lists (a study's fills, its alert conditions) as each item's changes. Such
 *   a list was sent whole, with the per-bar arrays its items hold (an RSI's gradient fills: 5 MB per update).
 * - A resumed run's bound: the bars it recalculated from (`from`), and that bar's time (`fromTime`). A long list of
 *   timed points in time order (a strategy's output values) is compared from that time on.
 * - SPLICE: a long list of timed points of several series one after another (markers, background colours of several
 *   calls), not in time order as a whole, as the points before `fromTime` it had and the new ones where they go.
 *
 * Kept apart from study-worker-types so the encodings of upstream's live updates stay as they are there.
 */
import { packedPrefix } from "./packed-points";

/** A resumed run's bound (encodeDelta): it recalculated the bars from `from`, of `bars`; `fromTime`, when given, is
 *  the time of bar `from`: the points of a list of timed points before it come from the bars before. */
export type DeltaBound = { from: number; bars: number; fromTime?: number };

export const ITEMS = "\u0000items";
export type Items = { [ITEMS]: unknown[] };
/** A list of timed points as the points before `before` it had (in their order) and the new ones: `runs` holds a count
 *  of the old points next, or the new points themselves, in the list's order. */
export const SPLICE = "\u0000splice";
export type Splice = { [SPLICE]: { before: number; runs: (number | unknown[])[] } };

/** Levels of objects and short lists encodeDelta and deltaSnapshot go down (a fill's gradient's values: 5). */
export const DEPTH = 6;
/** Longest list encoded item by item. */
export const SHORT = 64;
/** Bars before a resumed run's first recalculated bar that are still compared. A run gives the outputs of the bars
 *  before `from` of the previous update (lightweight-charts-indicators IndicatorRun.update: its per-bar series are
 *  written again from the forming bar, its other lists cut back to their length at the last confirmed bar), so they
 *  are the points sent; a few are compared still, for a study whose result reads a little around its bars. */
export const RESUMED_MARGIN = 64;

function isPlain(v: unknown): v is Record<string, unknown> {
  return v !== null && typeof v === "object" && Object.getPrototypeOf(v) === Object.prototype;
}

export const isItems = (d: unknown): d is Items => isPlain(d) && ITEMS in d;
export const isSplice = (d: unknown): d is Splice => isPlain(d) && SPLICE in d;

/** An item worth encoding by its changes: an object holding lists (a fill's colours, an alert's times), itself or
 *  in an object it holds. */
function holdsList(v: unknown): boolean {
  if (!isPlain(v)) return false;
  for (const k in v) {
    const x = v[k];
    if (Array.isArray(x)) return true;
    if (isPlain(x)) for (const j in x) if (Array.isArray(x[j])) return true;
  }
  return false;
}

/** Whether `list` goes item by item (ITEMS): a short list of items holding lists. */
export const goesByItems = (list: unknown[], depth: number): boolean => depth < DEPTH && list.length <= SHORT && list.some(holdsList);

const timeOf = (p: unknown): number | undefined => {
  const t = (p as { time?: unknown } | null)?.time;
  return typeof t === "number" ? t : undefined;
};

/** Index of the first point of `list` at or after `time`, when the points up to it have ascending numeric times
 *  (null: a list of other items, or not in time order). */
function timedPrefix(list: unknown[], time: number): number | null {
  let lo = 0;
  let hi = list.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    const t = timeOf(list[mid]);
    if (t === undefined) return null;
    if (t < time) lo = mid + 1;
    else hi = mid;
  }
  let before = -Infinity;
  for (let i = 0; i < lo; i++) {
    const t = timeOf(list[i]);
    if (t === undefined || t < before) return null;
    before = t;
  }
  return lo;
}

/** `next` as `prev`'s points before `time` and its own others, when both are lists of timed points with the same
 *  number of points before `time` (null otherwise). The points before `time` are taken to be the same (a resumed
 *  run's, specs/performance.md REQ-PERF-002.A6); the last ones are checked. */
function spliceOf(next: unknown[], prev: unknown[], time: number, same: (a: unknown, b: unknown) => boolean): Splice | null {
  const oldNext: unknown[] = [];
  const oldPrev: unknown[] = [];
  for (const p of next) {
    const t = timeOf(p);
    if (t === undefined) return null;
    if (t < time) oldNext.push(p);
  }
  for (const p of prev) {
    const t = timeOf(p);
    if (t === undefined) return null;
    if (t < time) oldPrev.push(p);
  }
  if (oldNext.length !== oldPrev.length || oldNext.length < 32) return null;
  for (let i = Math.max(0, oldNext.length - RESUMED_MARGIN); i < oldNext.length; i++) if (!same(oldNext[i], oldPrev[i])) return null;
  const runs: (number | unknown[])[] = [];
  for (const p of next) {
    const old = timeOf(p)! < time;
    const last = runs[runs.length - 1];
    if (old) {
      if (typeof last === "number") runs[runs.length - 1] = last + 1;
      else runs.push(1);
    } else if (Array.isArray(last)) last.push(p);
    else runs.push([p]);
  }
  return { [SPLICE]: { before: time, runs } };
}

/**
 * A resumed run's long list of timed points, bounded by the time of its first recalculated bar: the length of the
 * prefix it shares with `prev` (a list in time order holds new points, not changed in place: compared from a little
 * before that time), or a Splice (several series one after another). Null: not such a list, encoded as any other.
 */
export function encodeTimed(next: unknown[], prev: unknown[], bound: DeltaBound | undefined, same: (a: unknown, b: unknown) => boolean): { shared: number } | Splice | null {
  if (bound?.fromTime === undefined || next.length <= SHORT || next.length === bound.bars) return null;
  const at = timedPrefix(next, bound.fromTime);
  const inOrder = at !== null && prev.length >= at && (at === 0 || timeOf(prev[at - 1]) === timeOf(next[at - 1]));
  if (!inOrder) return spliceOf(next, prev, bound.fromTime, same);
  const n = Math.min(next.length, prev.length);
  const start = Math.max(0, at - RESUMED_MARGIN);
  let k = packedPrefix(next, prev, n, start);
  if (k === null) for (k = start; k < n && same(next[k], prev[k]); k++);
  return { shared: k };
}

/** The list a Splice gives from `prev` (`copy`: each new point copied). */
export function splicedList(s: Splice, prev: unknown[], copy: boolean): unknown[] {
  const { before, runs } = s[SPLICE];
  const old = prev.filter((p) => (timeOf(p) ?? Infinity) < before);
  const out: unknown[] = [];
  let at = 0;
  for (const run of runs) {
    if (typeof run === "number") {
      for (let i = 0; i < run; i++) out.push(old[at++]);
    } else for (const p of run) out.push(copy && isPlain(p) ? { ...p } : p);
  }
  return out;
}
