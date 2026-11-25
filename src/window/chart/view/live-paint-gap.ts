/*
 * How long a chart pane waits between two live paints (specs/performance.md REQ-PERF-004): a pane that is not the
 * focused one of a multi-chart layout paints at most 5 times a second, the focused pane, or the only one, at the
 * live rate. Kept apart from live.ts so upstream's live paint stays as it is there.
 */

/** Shortest time between two live paints of a pane that is not the focused one: 5 per second. */
export const BACKGROUND_PAINT_MS = 200;

/** The shortest time between two live paints of a pane: `liveMs` when it is focused or the only one (`active` not
 *  false), BACKGROUND_PAINT_MS when another pane is focused. */
export function paintGap(active: boolean | undefined, liveMs: number): number {
  return active === false ? BACKGROUND_PAINT_MS : liveMs;
}

/** The wait before the next live paint, after one at `paintedAt` (ms, the clock of `now`). */
export function paintWait(paintedAt: number, now: number, active: boolean | undefined, liveMs: number): number {
  return Math.max(0, paintedAt + paintGap(active, liveMs) - now);
}
