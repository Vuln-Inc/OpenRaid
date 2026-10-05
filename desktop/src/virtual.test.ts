import { describe, expect, it } from "vitest";
import { visibleRange } from "./virtual";

describe("virtualized ranges", () => {
  it("mounts a bounded window for 500 agents", () => {
    const range = visibleRange(500, 10000, 500, 64);
    expect(range.end - range.first).toBeLessThan(20);
    expect(range.totalHeight).toBe(32000);
  });
  it("clamps empty and end ranges", () => {
    expect(visibleRange(0, 0, 500, 64)).toEqual({ first: 0, end: 0, totalHeight: 0 });
    expect(visibleRange(5, 10000, 500, 64).first).toBe(5);
    expect(visibleRange(5, 0, 500, 64).end).toBe(5);
  });
  it("keeps every visible row mounted across scroll and viewport sizes", () => {
    for (const height of [240, 400, 900]) {
      for (const rowHeight of [64, 84, 100]) {
        const maxScroll = Math.max(0, 500 * rowHeight - height);
        for (const top of [0, 1, 127, 10000, maxScroll]) {
          const range = visibleRange(500, top, height, rowHeight);
          expect(range.first).toBeLessThanOrEqual(Math.floor(top / rowHeight));
          expect(range.end).toBeGreaterThanOrEqual(Math.min(500, Math.ceil((top + height) / rowHeight)));
          expect(range.end - range.first).toBeLessThanOrEqual(Math.ceil(height / rowHeight) + 11);
          expect(range.totalHeight).toBe(500 * rowHeight);
        }
      }
    }
  });
  it("maintains stable positions when live updates grow the roster", () => {
    const before = visibleRange(400, 10000, 400, 84);
    const after = visibleRange(500, 10000, 400, 84);
    expect(after.first).toBe(before.first);
    expect(after.end).toBe(before.end);
  });
});
