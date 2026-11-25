/* The live paint rate of a pane (specs/performance.md REQ-PERF-004.A1). */
import { describe, expect, it } from "vitest";
import { BACKGROUND_PAINT_MS, paintGap, paintWait } from "../live-paint-gap";

const LIVE = 33;

describe("a pane's live paints", () => {
  it("come every 33 ms on the focused pane or the only one, every 200 ms on another", () => {
    expect(paintGap(true, LIVE)).toBe(33);
    expect(paintGap(undefined, LIVE)).toBe(33);
    expect(paintGap(false, LIVE)).toBe(BACKGROUND_PAINT_MS);
    expect(BACKGROUND_PAINT_MS).toBe(200);
  });

  it("wait for what is left of the gap since the last paint, and not at all once it is over", () => {
    expect(paintWait(1000, 1010, true, LIVE)).toBe(23);
    expect(paintWait(1000, 1010, false, LIVE)).toBe(190);
    expect(paintWait(1000, 1300, false, LIVE)).toBe(0);
    // Nothing painted yet: the first paint goes at once.
    expect(paintWait(-Infinity, 5, false, LIVE)).toBe(0);
  });
});
