import { describe, expect, it } from "vitest";
import { applyLasso, cellsInPolygon, pointInPolygon } from "./viewer/lasso";

describe("lasso", () => {
  const square: [number, number][] = [[0, 0], [10, 0], [10, 10], [0, 10]];
  it("tests points against a polygon", () => {
    expect(pointInPolygon(5, 5, square)).toBe(true);
    expect(pointInPolygon(15, 5, square)).toBe(false);
    // concave "L": the notch is outside
    const l: [number, number][] = [[0, 0], [10, 0], [10, 4], [4, 4], [4, 10], [0, 10]];
    expect(pointInPolygon(2, 8, l)).toBe(true);
    expect(pointInPolygon(8, 8, l)).toBe(false);
  });
  it("selects centroids inside, skipping hidden cells", () => {
    const c = Float32Array.from([1, 1, 5, 5, 20, 20, 9, 9]);
    const colors = new Uint8Array(16).fill(255);
    colors[3 * 4 + 3] = 0; // cell 4 hidden
    expect(cellsInPolygon(c, colors, square)).toEqual([1, 2]);
    expect(cellsInPolygon(c, undefined, square)).toEqual([1, 2, 4]);
    expect(cellsInPolygon(c, colors, [[0, 0], [1, 1]])).toEqual([]);
  });
  it("replaces, adds to or removes from the selection", () => {
    const cur = new Set([1, 2, 3]);
    expect([...applyLasso(cur, [3, 4], "replace")]).toEqual([3, 4]);
    expect([...applyLasso(cur, [3, 4], "add")].sort()).toEqual([1, 2, 3, 4]);
    expect([...applyLasso(cur, [3, 4], "remove")].sort()).toEqual([1, 2]);
  });
});
