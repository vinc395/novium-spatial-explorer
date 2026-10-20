import { describe, expect, it } from "vitest";
import { SectionApi } from "./api";
import { initialState } from "./store";
import type { Manifest } from "./types";
import { cellLayers, type LayerCtx } from "./viewer/layers";

function manifest(withBoundaries: boolean): Manifest {
  const boundaries = { polygons: 2, vertices: 8, query_margin_um: 5, chunks: [], skipped_polygons: 0 };
  return {
    section_id: "S",
    bounds_um: [0, 0, 100, 100],
    grid: { tile_size_um: 256 },
    cells: { count: 2, boundaries: withBoundaries ? boundaries : null, nuclei: null, fields: [] },
  } as unknown as Manifest;
}

function layers(opts: { fill: boolean; outline?: boolean; centroids?: "auto" | "on" | "off"; overlay?: boolean; ovFill?: boolean; boundaries?: boolean }) {
  const m = manifest(opts.boundaries ?? true);
  const ctx: LayerCtx = {
    prefix: "p0",
    api: new SectionApi("S", "v1"),
    m,
    centroids: new Float32Array([1, 1, 2, 2]),
    cellColors: new Uint8Array(8).fill(255),
    colorVersion: 1,
    overlayColors: opts.overlay ? new Uint8Array(8).fill(200) : undefined,
    overlay: { ...initialState.overlay, source: opts.overlay ? "lr:x" : "", fill: opts.ovFill ?? true, outline: opts.ovFill === false },
    selection: new Set(),
    colorBy: initialState.colorBy,
    cells: { ...initialState.cells, fill: opts.fill, outline: opts.outline ?? false, centroids: opts.centroids ?? "auto" },
    transcripts: initialState.transcripts,
    genes: [],
    geneTileCounts: new Map(),
    grid: initialState.grid,
    ucellHeat: initialState.ucell.heat,
  };
  // "centroids" = the zoomed-out level of detail
  return cellLayers({ s: ctx, m, mode: "centroids", onStatus: () => {}, selectionVersion: 0 });
}
const ids = (opts: Parameters<typeof layers>[0]) => layers(opts).map((l) => l.id);
const hasDots = (l: string[]) => l.some((id) => id.startsWith("p0:centroids"));
const dots = (opts: Parameters<typeof layers>[0]) =>
  layers(opts).find((l) => l.id.startsWith("p0:centroids"))?.props as unknown as { filled: boolean; stroked: boolean } | undefined;

describe("zoomed-out cell dots", () => {
  it("follow the fill checkbox: colours switched off stay off when zooming out", () => {
    expect(dots({ fill: true })).toMatchObject({ filled: true, stroked: false });
    // Outline only: nothing zoomed out (one-pixel rings looked like filled dots).
    expect(hasDots(ids({ fill: false, outline: true }))).toBe(false);
    expect(hasDots(ids({ fill: false, outline: false }))).toBe(false);
  });
  it("show regardless of fill when centroids are set to always, and never when off", () => {
    expect(hasDots(ids({ fill: false, centroids: "on" }))).toBe(true);
    expect(dots({ fill: false, outline: true, centroids: "on" })).toMatchObject({ filled: false, stroked: true });
    expect(hasDots(ids({ fill: true, centroids: "off" }))).toBe(false);
  });
  it("stay when there are no cell outlines (the dots are the only cell shape)", () => {
    expect(hasDots(ids({ fill: false, boundaries: false }))).toBe(true);
  });
  it("leave the overlay dots to the overlay's own settings", () => {
    const l = ids({ fill: false, overlay: true });
    expect(hasDots(l)).toBe(false);
    expect(l).toContain("p0:overlay-centroids");
    // An outline-only overlay is not drawn as dots zoomed out either (unless "always").
    expect(ids({ fill: true, overlay: true, ovFill: false })).not.toContain("p0:overlay-centroids");
    expect(ids({ fill: true, overlay: true, ovFill: false, centroids: "on" })).toContain("p0:overlay-centroids");
  });
});
