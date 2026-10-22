import { describe, expect, it } from "vitest";
import { smoothFactor, smoothLabels, smoothValues } from "./gridSmooth";
import reference from "./gridSmooth.scipy.json";

const grid = (rows: string[]) => ({
  W: rows[0].length,
  H: rows.length,
  codes: Uint32Array.from(rows.join("").split("").map((c) => (c === "." ? 0 : Number(c)))),
});
const cell = (out: ArrayLike<number>, W: number, F: number, x: number, y: number) => out[(y * F + (F >> 1)) * W * F + x * F + (F >> 1)];
const bytes = (b64: string) => Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));

describe("smooth label grids", () => {
  it("matches SciPy: cubic-spline zoom of each label, then the largest (synthetic labels)", () => {
    // gridSmooth.scipy.json, written by scripts/grid_smooth_reference.py: SciPy's
    // ndimage.zoom(order=3, mode="grid-constant", grid_mode=True) per label (after
    // ndimage.gaussian_filter for the stronger settings) and an argmax with missing last,
    // on blobs with a ragged missing margin and single-point specks, at factors 5, 3 and 2.
    expect(reference.cases.map((c) => c.sigma)).toEqual([0, 0, 0, 0, 0.75, 2, 4, 1.25]);
    for (const c of reference.cases) {
      const out = smoothLabels(bytes(c.codes), c.W, c.H, c.F, c.sigma);
      const want = bytes(c.expected);
      let differ = 0;
      for (let i = 0; i < want.length; i++) if (out[i] !== want[i]) differ++;
      expect(differ, `${c.W}×${c.H} at ${c.F}×, sigma ${c.sigma}`).toBe(0);
    }
  });

  it("smooths more strongly with a Gaussian: small patches give way", () => {
    const g = grid(Array.from({ length: 9 }, (_, y) => (y === 4 ? "111121111" : "111111111")));
    expect(cell(smoothLabels(g.codes, g.W, g.H, 5), g.W, 5, 4, 4)).toBe(2); // the spline keeps it
    expect(new Set(smoothLabels(g.codes, g.W, g.H, 5, 0.75))).toEqual(new Set([1]));
    const u = grid(Array.from({ length: 7 }, () => "2222222"));
    expect([...new Set(smoothLabels(u.codes, u.W, u.H, 4, 2))]).toEqual([2]);
  });

  it("keeps every grid point's own label at its centre (the spline interpolates)", () => {
    const g = grid(["11122", "11222", "13322", "..322", "...22"]);
    const out = smoothLabels(g.codes, g.W, g.H, 5);
    for (let y = 0; y < g.H; y++) for (let x = 0; x < g.W; x++) expect(cell(out, g.W, 5, x, y)).toBe(g.codes[y * g.W + x]);
  });

  it("leaves uniform grids and factor 1 unchanged", () => {
    const g = grid(Array.from({ length: 7 }, () => "2222222"));
    expect([...new Set(smoothLabels(g.codes, g.W, g.H, 4))]).toEqual([2]);
    const h = grid(["12", "21"]);
    expect([...smoothLabels(h.codes, h.W, h.H, 1)]).toEqual([...h.codes]);
  });

  it("turns a staircase edge into a smooth one", () => {
    // A diagonal edge drawn as squares jumps a whole cell (F pixels) every F rows; smoothed,
    // the boundary moves by a pixel or two per row.
    const g = grid(Array.from({ length: 8 }, (_, y) => "1".repeat(y + 1) + "2".repeat(7 - y)));
    const F = 5, OW = g.W * F;
    const out = smoothLabels(g.codes, g.W, g.H, F);
    const edge = (row: number) => {
      let x = 0;
      while (x < OW && out[row * OW + x] === 1) x++;
      return x;
    };
    const steps = [];
    for (let row = 2 * F; row < 6 * F; row++) steps.push(edge(row + 1) - edge(row));
    expect(Math.min(...steps)).toBeGreaterThanOrEqual(0);
    expect(Math.max(...steps)).toBeLessThanOrEqual(2);
    expect(steps.reduce((a, b) => a + b, 0)).toBe(4 * F); // still one cell per cell
  });

  it("chooses about 2 µm per pixel within a pixel and texture budget", () => {
    expect(smoothFactor(800, 520, 10)).toBe(5);
    expect(smoothFactor(400, 260, 20)).toBe(10);
    expect(smoothFactor(800, 500, 20)).toBe(5); // 4096 pixels per side at most
    expect(smoothFactor(3000, 2000, 10)).toBe(1);
    expect(smoothFactor(2000, 2000, 10, 16_000_000, 1e9)).toBe(2); // 16M pixels at most
    expect(smoothFactor(100, 100, 1)).toBe(1);
  });
});

describe("smooth numeric grids", () => {
  it("passes through the grid values and keeps a ramp straight", () => {
    const W = 24, H = 12, F = 5, OW = W * F; // odd F: a pixel sits on each grid point
    const values = Float32Array.from({ length: W * H }, (_, i) => (i % W) * 0.5 + 1);
    const out = smoothValues(values, W, H, F);
    for (let y = 0; y < H; y++) for (let x = 0; x < W; x++) expect(cell(out, W, F, x, y)).toBeCloseTo(values[y * W + x], 4);
    // The spline reproduces straight lines away from the grid's edge.
    for (let ox = 6 * F; ox < (W - 6) * F; ox++) expect(out[6 * F * OW + ox]).toBeCloseTo(((ox + 0.5) / F - 0.5) * 0.5 + 1, 3);
  });

  it("keeps a constant field constant when blurred", () => {
    const W = 12, H = 9, F = 3;
    const values = Float32Array.from({ length: W * H }, (_, i) => (i % W < 2 ? NaN : 3));
    const out = smoothValues(values, W, H, F, 2);
    for (const v of out) if (Number.isFinite(v)) expect(v).toBeCloseTo(3, 4);
    expect(out.some((v) => Number.isFinite(v))).toBe(true);
  });

  it("is missing where missing would win as a label", () => {
    const W = 8, H = 4, F = 4, OW = W * F;
    const values = Float32Array.from({ length: W * H }, (_, i) => (i % W < 4 ? 2 : NaN));
    const out = smoothValues(values, W, H, F);
    const row = 2 * F;
    expect(out[row * OW + 2 * F]).toBeCloseTo(2, 4);
    expect(Number.isNaN(out[row * OW + 5 * F])).toBe(true);
    // The edge falls halfway between the last point present and the first missing one.
    let x = 0;
    while (Number.isFinite(out[row * OW + x])) x++;
    expect(x).toBe(4 * F);
    const same = smoothValues(values, W, H, 1);
    values.forEach((v, i) => (Number.isNaN(v) ? expect(same[i]).toBeNaN() : expect(same[i]).toBeCloseTo(v, 4)));
  });
});
