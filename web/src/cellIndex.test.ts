import { describe, expect, it } from "vitest";
import { nearestCellKey } from "./viewer/cellIndex";

describe("nearest cell to a click", () => {
  // Keys 1..4 at (0,0), (10,0), (0,10), (50,50) µm.
  const c = Float32Array.from([0, 0, 10, 0, 0, 10, 50, 50]);

  it("finds the closest centroid within the radius", () => {
    expect(nearestCellKey(c, 1, 1, 6)).toBe(1);
    expect(nearestCellKey(c, 8, 1, 6)).toBe(2);
    expect(nearestCellKey(c, 1, 9, 6)).toBe(3);
    expect(nearestCellKey(c, 48, 52, 6)).toBe(4);
  });

  it("finds nothing farther than the radius", () => {
    expect(nearestCellKey(c, 30, 30, 6)).toBeUndefined();
    expect(nearestCellKey(c, -7, 0, 6)).toBeUndefined();
  });

  it("skips cells drawn transparent (hidden categories)", () => {
    const colors = new Uint8Array(16).fill(255);
    colors[3] = 0; // cell 1 hidden
    expect(nearestCellKey(c, 2, 1, 12, colors)).toBe(2);
    expect(nearestCellKey(c, 2, 1, 12)).toBe(1);
  });
});
