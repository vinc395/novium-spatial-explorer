// Pure layer builders. World coordinates are Xenium µm (y down). Images keep
// their native pixel frame and are placed with modelMatrix = matrix_xu_from_px.

import type { Layer } from "@deck.gl/core";
import { COORDINATE_SYSTEM } from "@deck.gl/core";
import { BitmapLayer, PathLayer, ScatterplotLayer, SolidPolygonLayer } from "@deck.gl/layers";
import { TileLayer } from "@deck.gl/geo-layers";
import { DataFilterExtension } from "@deck.gl/extensions";
import { getBuffer, type SectionApi } from "../api";
import { decodeDensity, decodePoints, decodePolygons, tileKey, POINT_FLAG_OVERLAPS_NUCLEUS, QV_SCALE, type PolygonChunk } from "../codecs";
import { hexToRgba } from "../colors";
import type { AppState, GridImage, UcellImage } from "../store";
import type { ImageInfo, Manifest } from "../types";
import { ViewportTileLayer, type TileStatus } from "./ViewportTileLayer";

export type CellMode = "polygons" | "centroids";
export type TxMode = "points" | "density" | "off";

/** 4x4 column-major model matrix from a 2-D affine `[[a,b,c],[d,e,f],[0,0,1]]`. */
/** Everything a panel's layers need: that panel's section data + the shared settings.
 *  `prefix` (e.g. "p0") starts every layer id so each view draws only its own panel. */
export interface LayerCtx {
  prefix: string;
  api: SectionApi;
  m: Manifest;
  centroids?: Float32Array;
  cellColors?: Uint8Array;
  colorVersion: number;
  /** overlay colours (alpha 0 = not drawn) */
  overlayColors?: Uint8Array;
  overlay: AppState["overlay"];
  selection: Set<number>;
  selected?: number;
  colorBy: AppState["colorBy"];
  cells: AppState["cells"];
  transcripts: AppState["transcripts"];
  genes: { id: number; color: string }[];
  geneTileCounts: Map<number, Map<number, number>>;
  gridImage?: GridImage;
  grid: AppState["grid"];
  ucellImage?: UcellImage;
  ucellHeat: AppState["ucell"]["heat"];
  /** dark theme in effect (canvas colours switch for contrast) */
  dark?: boolean;
}

export function affineToMatrix4(m: number[][]): number[] {
  return [m[0][0], m[1][0], 0, 0, m[0][1], m[1][1], 0, 0, 0, 0, 1, 0, m[0][2], m[1][2], 0, 1];
}

async function loadBitmap(url: string, signal?: AbortSignal): Promise<ImageBitmap | null> {
  const r = await fetch(url, { signal });
  if (!r.ok) return null;
  return createImageBitmap(await r.blob());
}

const closeBitmap = (tile: { content?: unknown }) => {
  const c = tile.content as ImageBitmap | null | undefined;
  if (c && typeof c.close === "function") c.close();
};

/** `tileBoost`: extra levels of detail (log2 of the image-export scale). */
export function imageLayer(prefix: string, api: SectionApi, img: ImageInfo, visible: boolean, opacity: number, tileBoost = 0): Layer {
  const reg = img.registration;
  const dpr = Math.min(2, window.devicePixelRatio || 1);
  const W = img.width, H = img.height;
  return new TileLayer<ImageBitmap | null>({
    // Section + version in the id: a new section can never reuse the old tileset.
    id: `${prefix}:image-${api.sectionId}@${api.version}-${img.id}`,
    onTileUnload: closeBitmap,
    visible,
    opacity,
    tileSize: img.tile_size,
    minZoom: -(img.levels - 1),
    maxZoom: 0,
    extent: [0, 0, W, H],
    modelMatrix: affineToMatrix4(reg.matrix_xu_from_px) as never,
    // z = ceil(viewZoom + offset): choose the level whose texels are ~1 device pixel.
    zoomOffset: Math.log2(reg.approx_pixel_size_um) + Math.log2(dpr) + tileBoost,
    maxRequests: 12,
    debounceTime: 20,
    refinementStrategy: "best-available",
    getTileData: ({ index, signal }) => {
      // With a rotated/mirrored modelMatrix the indexer can propose tiles just
      // outside the image; skip them instead of requesting invalid URLs.
      const level = -index.z;
      const span = img.tile_size * 2 ** level;
      if (index.x < 0 || index.y < 0 || index.x * span >= W || index.y * span >= H || level < 0 || level >= img.levels) {
        return Promise.resolve(null);
      }
      return loadBitmap(api.imageTileUrl(img.id, level, index.x, index.y), signal);
    },
    renderSubLayers: (props) => {
      const { left, top, right, bottom } = props.tile.bbox as { left: number; top: number; right: number; bottom: number };
      if (!props.data) return null;
      return new BitmapLayer({
        id: props.id,
        image: props.data,
        bounds: [left, Math.min(bottom, H), Math.min(right, W), top],
        opacity: props.opacity,
        visible: props.visible,
        modelMatrix: props.modelMatrix,
        pickable: false,
      });
    },
  });
}

/** Stable deck.gl binary data objects per decoded chunk (built once, reused across renders).
 *  Outlines need explicitly closed rings: with binary paths deck draws `loop` paths open. */
const polyCache = new WeakMap<PolygonChunk, { fill: object; outline: object }>();
function polyData(c: PolygonChunk) {
  let d = polyCache.get(c);
  if (!d) {
    const n = c.cellKeys.length;
    const closedStarts = new Uint32Array(n + 1);
    const closed = new Float32Array(c.xy.length + 2 * n);
    let o = 0;
    for (let i = 0; i < n; i++) {
      const a = c.starts[i], b = c.starts[i + 1];
      closedStarts[i] = o / 2;
      closed.set(c.xy.subarray(2 * a, 2 * b), o);
      o += 2 * (b - a);
      closed[o++] = c.xy[2 * a];
      closed[o++] = c.xy[2 * a + 1];
    }
    closedStarts[n] = o / 2;
    d = {
      fill: { length: n, startIndices: c.starts, attributes: { getPolygon: { value: c.xy, size: 2 } } },
      outline: { length: n, startIndices: closedStarts, attributes: { getPath: { value: closed, size: 2 } } },
    };
    polyCache.set(c, d);
  }
  return d;
}

const centroidCache = new WeakMap<Uint8Array, object>();
function centroidData(centroids: Float32Array, colors: Uint8Array) {
  let d = centroidCache.get(colors);
  if (!d) {
    d = {
      length: centroids.length / 2,
      attributes: {
        getPosition: { value: centroids, size: 2 },
        getFillColor: { value: colors, size: 4, normalized: true },
        getLineColor: { value: colors, size: 4, normalized: true },
      },
    };
    centroidCache.set(colors, d);
  }
  return d;
}

export interface CellLayerArgs {
  s: LayerCtx;
  m: Manifest;
  mode: CellMode;
  onStatus: (s: TileStatus) => void;
  selectionVersion: number;
}

const OUTLINE_DEFAULT: [number, number, number, number] = [30, 30, 30, 220];
const OUTLINE_DEFAULT_DARK: [number, number, number, number] = [225, 228, 235, 220];
const NUCLEUS: [number, number, number, number] = [40, 90, 255, 230];
const NUCLEUS_DARK: [number, number, number, number] = [120, 165, 255, 235];
const SELECTED: [number, number, number, number] = [255, 214, 0, 255];

/**
 * Radius (µm) of the zoomed-out cell dots: the section's typical cell (from its mean
 * cell area) enlarged by 20%, so neighbouring dots close up like the cell outlines do.
 */
export function cellDotRadius(m: Manifest): number {
  const area = m.cells?.fields.find((f) => f.id === "cell_area")?.stats?.mean;
  const r = area && area > 0 ? Math.sqrt(area / Math.PI) : 4.5;
  return Math.min(15, Math.max(2, r * 1.2));
}

export function cellLayers({ s, m, mode, onStatus, selectionVersion }: CellLayerArgs): Layer[] {
  const api = s.api!;
  const cells = m.cells;
  const colors = s.cellColors;
  if (!cells || !colors || !s.centroids) return [];
  const out: Layer[] = [];
  const grid = m.grid.tile_size_um;
  const { fill, outline, opacity, outlineWidth } = s.cells;
  const sel = s.selection;
  const colorOf = (key: number, target: number[] | Uint8ClampedArray | Float32Array, alpha?: number) => {
    const o = (key - 1) * 4;
    target[0] = colors[o];
    target[1] = colors[o + 1];
    target[2] = colors[o + 2];
    target[3] = alpha ?? colors[o + 3];
    return target;
  };
  // The overlay is its own layer above the base colouring, reusing the same tiles.
  const ov = s.overlay;
  const ovColors = ov.source ? s.overlayColors : undefined;
  const ovFill = !!ovColors && ov.fill;
  const ovLine = !!ovColors && ov.outline;
  const ovOf = (key: number, target: number[]) => {
    const o = (key - 1) * 4;
    target[0] = ovColors![o];
    target[1] = ovColors![o + 1];
    target[2] = ovColors![o + 2];
    target[3] = ovColors![o + 3];
    return target;
  };
  // Zoomed out, cells are drawn at their centroids ("auto") as filled dots when "fill" is
  // on and not at all when it is off: an outline cannot be seen at that scale, and rings
  // a pixel wide looked like the colours switching back on. "Always" draws them regardless
  // (rings when only "outline" is on). Without boundaries the dots are the only cell geometry.
  const dotR = cellDotRadius(m);
  const dotZoom =
    s.cells.centroids === "on" || (s.cells.centroids === "auto" && (mode === "centroids" || !cells.boundaries));
  const dotFill = fill || !cells.boundaries || (s.cells.centroids === "on" && !outline);
  const showCentroids = s.cells.centroids === "on" || (dotZoom && (fill || !cells.boundaries));
  // With fill and outline off, the tiles still load while cells are selected, to outline them.
  const hasSel = sel.size > 0 || !!s.selected;
  if (mode === "polygons" && cells.boundaries && (fill || outline || ovFill || ovLine || hasSel)) {
    out.push(
      new ViewportTileLayer<PolygonChunk>({
        id: `${s.prefix}:cells-poly`,
        tileSize: grid,
        margin: cells.boundaries.query_margin_um,
        extent: m.bounds_um,
        cacheKey: `${m.section_id}|${api.version}|cell`,
        fetchTile: async (tx, ty, signal) => decodePolygons(await getBuffer(api.chunkUrl("cell", tx, ty), signal)),
        renderVersion: `${s.colorVersion}|${selectionVersion}|${s.selected}|${s.colorBy.source}|${fill}|${outline}|${opacity}|${outlineWidth}|${ovFill}|${ovLine}|${ov.opacity}|${ov.outlineWidth}|${s.dark}`,
        maxVisibleTiles: 150,
        maxCachedTiles: 400,
        maxConcurrent: 6,
        onStatus,
        renderTile: (t) => {
          const c = t.data;
          const n = c.cellKeys.length;
          if (!n) return null;
          const pd = polyData(c);
          const layers: Layer[] = [];
          if (fill) {
            layers.push(
              new SolidPolygonLayer({
                id: `fill-${t.key}`,
                data: pd.fill as never,
                _normalize: false,
                filled: true,
                getFillColor: ((_: unknown, { index, target }: { index: number; target: number[] }) => colorOf(c.cellKeys[index], target)) as never,
                opacity,
                pickable: true,
                autoHighlight: true,
                highlightColor: [255, 255, 255, 90],
                updateTriggers: { getFillColor: s.colorVersion },
                cellKeys: c.cellKeys,
              } as never),
            );
          }
          if (outline) {
            layers.push(
              new PathLayer({
                id: `outline-${t.key}`,
                data: pd.outline as never,
                _pathType: "open",
                widthUnits: "pixels",
                getWidth: ((_: unknown, { index }: { index: number }) => (sel.has(c.cellKeys[index]) ? 3 : outlineWidth)) as never,
                getColor: ((_: unknown, { index, target }: { index: number; target: number[] }) => {
                  const k = c.cellKeys[index];
                  if (sel.has(k) || s.selected === k) return SELECTED;
                  return s.colorBy.source ? colorOf(k, target, colors[(k - 1) * 4 + 3] ? 255 : 0) : s.dark ? OUTLINE_DEFAULT_DARK : OUTLINE_DEFAULT;
                }) as never,
                pickable: !fill,
                updateTriggers: { getColor: [s.colorVersion, selectionVersion, s.selected, s.dark], getWidth: [selectionVersion, outlineWidth] },
                cellKeys: c.cellKeys,
              } as never),
            );
          }
          if (ovFill) {
            layers.push(
              new SolidPolygonLayer({
                id: `ovfill-${t.key}`,
                data: pd.fill as never,
                _normalize: false,
                filled: true,
                getFillColor: ((_: unknown, { index, target }: { index: number; target: number[] }) => ovOf(c.cellKeys[index], target)) as never,
                opacity: ov.opacity,
                pickable: false,
                updateTriggers: { getFillColor: s.colorVersion },
              } as never),
            );
          }
          if (ovLine) {
            layers.push(
              new PathLayer({
                id: `ovline-${t.key}`,
                data: pd.outline as never,
                _pathType: "open",
                widthUnits: "pixels",
                getWidth: ov.outlineWidth,
                getColor: ((_: unknown, { index, target }: { index: number; target: number[] }) => {
                  const k = c.cellKeys[index];
                  // Leave the selection highlight of the base outline visible.
                  if (outline && (sel.has(k) || s.selected === k)) return [0, 0, 0, 0];
                  return ovOf(k, target);
                }) as never,
                pickable: false,
                updateTriggers: { getColor: [s.colorVersion, selectionVersion, s.selected] },
              } as never),
            );
          }
          if (!outline && hasSel) {
            // The selected cells' outlines, on top (the full outline layer draws them otherwise).
            const paths: [number, number][][] = [];
            for (let i = 0; i < n; i++) {
              const k = c.cellKeys[i];
              if (!sel.has(k) && s.selected !== k) continue;
              const ring: [number, number][] = [];
              for (let j = c.starts[i]; j < c.starts[i + 1]; j++) ring.push([c.xy[2 * j], c.xy[2 * j + 1]]);
              if (ring.length) ring.push(ring[0]);
              paths.push(ring);
            }
            if (paths.length)
              layers.push(
                new PathLayer<[number, number][]>({
                  id: `sel-${t.key}`,
                  data: paths,
                  getPath: (d) => d,
                  widthUnits: "pixels",
                  getWidth: 3,
                  getColor: SELECTED,
                  pickable: false,
                }),
              );
          }
          return layers;
        },
      }),
    );
  }
  if (s.cells.nuclei && cells.nuclei && mode === "polygons") {
    out.push(
      new ViewportTileLayer<PolygonChunk>({
        id: `${s.prefix}:nuclei`,
        tileSize: grid,
        margin: cells.nuclei.query_margin_um,
        extent: m.bounds_um,
        cacheKey: `${m.section_id}|${api.version}|nucleus`,
        fetchTile: async (tx, ty, signal) => decodePolygons(await getBuffer(api.chunkUrl("nucleus", tx, ty), signal)),
        renderVersion: s.dark ? 1 : 0,
        maxVisibleTiles: 150,
        maxCachedTiles: 400,
        maxConcurrent: 4,
        renderTile: (t) =>
          t.data.cellKeys.length
            ? new PathLayer({
                id: `nuc-${t.key}`,
                data: polyData(t.data).outline as never,
                _pathType: "open",
                widthUnits: "pixels",
                getWidth: 1,
                getColor: s.dark ? NUCLEUS_DARK : NUCLEUS,
              } as never)
            : null,
      }),
    );
  }
  if (showCentroids) {
    out.push(
      new ScatterplotLayer({
        id: `${s.prefix}:centroids-${m.section_id}@${api.version}`,
        data: centroidData(s.centroids, colors) as never,
        isCentroids: true,
        radiusUnits: "common",
        getRadius: dotR,
        radiusMinPixels: 1.2,
        radiusMaxPixels: 10,
        filled: dotFill,
        stroked: !dotFill,
        lineWidthUnits: "pixels",
        getLineWidth: outlineWidth,
        opacity: Math.max(opacity, 0.85),
        pickable: true,
      } as never),
    );
  }
  // The overlay's zoomed-out dots follow its own fill the same way.
  if (dotZoom && (ovFill || (ovLine && s.cells.centroids === "on"))) {
    // Zoomed out: a smaller overlay dot inside each cell's dot, so both colours show.
    out.push(
      new ScatterplotLayer({
        id: `${s.prefix}:overlay-centroids`,
        data: centroidData(s.centroids, ovColors!) as never,
        radiusUnits: "common",
        getRadius: dotR * 0.6,
        radiusMinPixels: 0.8,
        radiusMaxPixels: 6,
        opacity: ovFill ? Math.max(ov.opacity, 0.6) : 1,
        pickable: false,
      } as never),
    );
  }
  if (sel.size || s.selected) {
    const keys = [...sel];
    if (s.selected && !sel.has(s.selected)) keys.push(s.selected);
    out.push(
      new ScatterplotLayer<number>({
        id: `${s.prefix}:selection`,
        data: keys,
        getPosition: (k) => [s.centroids![(k - 1) * 2], s.centroids![(k - 1) * 2 + 1]],
        radiusUnits: "common",
        getRadius: 9,
        stroked: true,
        filled: false,
        lineWidthUnits: "pixels",
        getLineWidth: 2,
        getLineColor: SELECTED,
        radiusMinPixels: 5,
        updateTriggers: { getPosition: selectionVersion },
      }),
    );
  }
  return out;
}

export interface PointsData {
  count: number;
  buffer: ArrayBuffer;
  recordOffset: number;
  geneIds: Uint32Array; // per point
  filter: Float32Array; // [qv, assigned] per point
  segments: { geneId: number; count: number }[];
  /** stable deck.gl data object (positions + filter values), built once */
  deck?: object;
}

const EMPTY_POINTS: PointsData = { count: 0, buffer: new ArrayBuffer(0), recordOffset: 0, geneIds: new Uint32Array(0), filter: new Float32Array(0), segments: [] };

function preparePoints(buf: ArrayBuffer): PointsData {
  const p = decodePoints(buf);
  const geneIds = new Uint32Array(p.count);
  const filter = new Float32Array(p.count * 2);
  let i = 0;
  for (const s of p.segments) {
    geneIds.fill(s.geneId, i, i + s.count);
    i += s.count;
  }
  const dv = p.records;
  for (let k = 0; k < p.count; k++) {
    filter[2 * k] = dv.getUint8(k * 16 + 14) / QV_SCALE;
    filter[2 * k + 1] = dv.getUint32(k * 16 + 8, true) !== 0 ? 1 : 0;
  }
  return { count: p.count, buffer: p.buffer, recordOffset: p.recordOffset, geneIds, filter, segments: p.segments };
}

export function pointDetails(d: PointsData, index: number) {
  const dv = new DataView(d.buffer, d.recordOffset);
  const o = index * 16;
  return {
    geneId: d.geneIds[index],
    x: dv.getFloat32(o, true),
    y: dv.getFloat32(o + 4, true),
    cellKey: dv.getUint32(o + 8, true),
    z: dv.getInt16(o + 12, true) / 100,
    qv: dv.getUint8(o + 14) / QV_SCALE,
    overlapsNucleus: (dv.getUint8(o + 15) & POINT_FLAG_OVERLAPS_NUCLEUS) !== 0,
  };
}

export interface TxArgs {
  s: LayerCtx;
  m: Manifest;
  mode: TxMode;
  onStatus: (s: TileStatus) => void;
}

export function transcriptLayers({ s, m, mode, onStatus }: TxArgs): Layer[] {
  const t = m.transcripts;
  const api = s.api!;
  if (!t || !s.transcripts.visible || !s.genes.length || mode === "off") return [];
  const ids = s.genes.map((g) => g.id).sort((a, b) => a - b);
  const colorKey = s.genes.map((g) => `${g.id}:${g.color}`).join(",");
  const colorById = new Map(s.genes.map((g) => [g.id, hexToRgba(g.color)]));
  const { qvMin, showUnassigned, pointSize, opacity } = s.transcripts;
  if (mode === "points") {
    return [
      new ViewportTileLayer<PointsData>({
        id: `${s.prefix}:tx-points`,
        tileSize: m.grid.tile_size_um,
        extent: m.bounds_um,
        cacheKey: `${m.section_id}|${api.version}|${ids.join(",")}`,
        fetchTile: async (tx, ty, signal) => {
          // Skip the request only when every gene's tile counts are known and all are zero.
          const k = tileKey(tx, ty);
          const counts = ids.map((g) => s.geneTileCounts.get(g));
          if (counts.every((c) => c && !c.has(k))) return EMPTY_POINTS;
          return preparePoints(await getBuffer(api.pointsUrl(tx, ty, ids), signal));
        },
        renderVersion: `${colorKey}|${qvMin}|${showUnassigned}|${pointSize}|${opacity}`,
        maxVisibleTiles: 160,
        maxCachedTiles: 300,
        maxConcurrent: 6,
        onStatus,
        renderTile: (tile) => {
          const d = tile.data;
          if (!d.count) return null;
          d.deck ??= {
            length: d.count,
            attributes: {
              getPosition: { value: new Float32Array(d.buffer), size: 2, stride: 16, offset: d.recordOffset },
              getFilterValue: { value: d.filter, size: 2 },
            },
          };
          const white: [number, number, number, number] = [255, 255, 255, 255];
          return new ScatterplotLayer({
            id: `pts-${tile.key}`,
            data: d.deck as never,
            getFillColor: ((_: unknown, { index }: { index: number }) => colorById.get(d.geneIds[index]) ?? white) as never,
            updateTriggers: { getFillColor: colorKey },
            radiusUnits: "pixels",
            getRadius: pointSize / 2,
            opacity,
            pickable: true,
            extensions: [new DataFilterExtension({ filterSize: 2 })],
            filterRange: [
              [qvMin, 1000],
              [showUnassigned ? 0 : 1, 1],
            ],
            pointsData: d,
          } as never);
        },
      }),
    ];
  }
  // Aggregated density per gene (server-side binning; QV filter applied server-side).
  const b = m.bounds_um;
  const area = Math.max(1, (b[2] - b[0]) * (b[3] - b[1]));
  return s.genes.map((g) => {
    const info = t.genes[g.id];
    const rgba = hexToRgba(g.color);
    return new TileLayer<ImageBitmap | null>({
      id: `${s.prefix}:density-${m.section_id}@${api.version}-${g.id}`,
      onTileUnload: closeBitmap,
      tileSize: t.density_tile_bins,
      minZoom: -t.density_max_level,
      maxZoom: 0,
      // ~2 CSS px per bin keeps the number of density requests modest.
      zoomOffset: -1,
      extent: b,
      opacity,
      maxRequests: 8,
      refinementStrategy: "best-available",
      updateTriggers: { getTileData: [g.color, qvMin] },
      getTileData: async ({ index, signal }) => {
        const level = -index.z;
        const d = decodeDensity(await getBuffer(api.densityUrl(g.id, level, index.x, index.y, qvMin), signal));
        if (!d.total) return null;
        const binArea = 4 ** level;
        const ref = Math.max(2, (6 * (info?.count ?? 1) * binArea) / area);
        const img = new ImageData(d.width, d.height);
        for (let i = 0; i < d.counts.length; i++) {
          const c = d.counts[i];
          if (!c) continue;
          img.data[i * 4] = rgba[0];
          img.data[i * 4 + 1] = rgba[1];
          img.data[i * 4 + 2] = rgba[2];
          img.data[i * 4 + 3] = Math.round(70 + 185 * Math.min(1, c / ref));
        }
        return createImageBitmap(img);
      },
      renderSubLayers: (props) => {
        if (!props.data) return null;
        const { left, top, right, bottom } = props.tile.bbox as { left: number; top: number; right: number; bottom: number };
        return new BitmapLayer({
          id: props.id,
          image: props.data,
          bounds: [left, bottom, right, top],
          opacity: props.opacity,
          textureParameters: { minFilter: "nearest", magFilter: "nearest" },
          pickable: false,
        });
      },
    });
  });
}

export function gridLayer(s: LayerCtx): Layer | null {
  const gi = s.gridImage;
  if (!gi || !s.grid.visible) return null;
  const g = gi.grid;
  const [ox, oy] = g.origin_um;
  const W = g.width * g.spacing_um, H = g.height * g.spacing_um;
  return new BitmapLayer({
    id: `${s.prefix}:grid-${gi.key}`,
    image: gi.bitmap,
    bounds: [ox, oy + H, ox + W, oy],
    opacity: s.grid.opacity,
    textureParameters: { minFilter: "nearest", magFilter: "nearest" },
    coordinateSystem: COORDINATE_SYSTEM.CARTESIAN,
    pickable: true,
  });
}

/** UCell heatmap: a bin-level result, or cell-level scores averaged per bin. */
export function ucellHeatLayer(s: LayerCtx): Layer | null {
  const ui = s.ucellImage;
  if (!ui || !s.ucellHeat.visible) return null;
  const g = ui.grid;
  const [ox, oy] = g.origin_um;
  const W = g.width * g.bin_um, H = g.height * g.bin_um;
  return new BitmapLayer({
    id: `${s.prefix}:ucell-heat`,
    image: ui.bitmap,
    bounds: [ox, oy + H, ox + W, oy],
    opacity: s.ucellHeat.opacity,
    textureParameters: { minFilter: "nearest", magFilter: "nearest" },
    coordinateSystem: COORDINATE_SYSTEM.CARTESIAN,
    pickable: false,
  });
}
