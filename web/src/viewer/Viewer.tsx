import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import DeckGL, { type DeckGLRef } from "@deck.gl/react";
import { OrthographicView, type Layer, type PickingInfo } from "@deck.gl/core";
import { activePanel, getState, morphSettings, panelGenes, setFolds, setPanel, setSidePanel, setState, useStore, type PanelData } from "../store";
import { getStatus, setStatus } from "../statusBus";
import { tileKey } from "../codecs";
import { closeSlot, openSection } from "../data";
import { cellDotRadius, cellLayers, gridLayer, imageLayer, ucellHeatLayer, pointDetails, transcriptLayers, type CellMode, type LayerCtx, type PointsData, type TxMode } from "./layers";
import { nearestCellKey } from "./cellIndex";
import type { TileStatus } from "./ViewportTileLayer";
import { sectionLabel } from "../types";
import { LR_GROUPS } from "../lr";
import { systemDarkQuery } from "../theme";
import { applyLasso, cellsInPolygon } from "./lasso";
import { morphologyLayer } from "./morphology";

interface VS {
  target: [number, number, number];
  zoom: number;
  minZoom?: number;
  maxZoom?: number;
}

const POINT_BUDGET = 1_500_000;
/** columns x rows for 1-4 panels */
const LAYOUT: Record<number, [number, number]> = { 1: [1, 1], 2: [2, 1], 3: [3, 1], 4: [2, 2] };
export const PANEL_LAYOUT = LAYOUT;
const viewId = (slot: number) => `p${slot}`;

/** One rendered frame for an image export (all panels, in device pixels of the copy). */
export interface Capture {
  canvas: HTMLCanvasElement;
  /** device pixels per CSS pixel in `canvas` */
  ratio: number;
  /** each panel's rectangle in `canvas` ([x, y, w, h]) and its µm per pixel there */
  panels: { slot: number; sectionId?: string; rect: [number, number, number, number]; umPerPx: number }[];
}

export const viewerApi: {
  fit?: () => void;
  flyTo?: (x: number, y: number, zoom?: number, durationMs?: number) => void;
  /** Render at `scale` × the screen's pixels (finer image tiles too), wait for every tile and copy the frame. */
  capture?: (scale: number, onProgress?: (msg: string) => void) => Promise<Capture>;
} = {};

/** Longest side of an exported frame (GPU and memory limits). */
export const MAX_EXPORT_SIDE = 8192;

let deckHandle: DeckGLRef<OrthographicView[]> | null = null;
let cameras: Record<string, VS> = {};
// Debug/test hook (camera control, status and picking only; no data access).
(window as unknown as { __xenium?: unknown }).__xenium = {
  viewerApi,
  status: () => getStatus(),
  zooms: () => Object.fromEntries(Object.entries(cameras).map(([k, v]) => [k, v.zoom])),
  targets: () => Object.fromEntries(Object.entries(cameras).map(([k, v]) => [k, [v.target[0], v.target[1]]])),
  layerIds: () => ((deckHandle?.deck?.props.layers ?? []) as { id: string }[]).map((l) => l.id),
  /** opacity of the first top-level layer whose id contains `part` */
  layerOpacity: (part: string) =>
    ((deckHandle?.deck?.props.layers ?? []) as { id: string; props: { opacity?: number } }[]).find((l) => l.id.includes(part))?.props.opacity,
  /** every rendered layer id, including tile sublayers */
  allLayerIds: () =>
    ((deckHandle?.deck as unknown as { layerManager?: { getLayers: () => { id: string }[] } })?.layerManager?.getLayers() ?? []).map((l) => l.id),
  /** For rendered layers whose id contains `part`: [id, instances whose fill alpha > 0, instances]. */
  visibleFill: (part: string) =>
    (
      (deckHandle?.deck as unknown as { layerManager?: { getLayers: () => DebugLayer[] } })?.layerManager?.getLayers() ?? []
    )
      .filter((l) => l.id.includes(part))
      .map((l) => {
        const a = l.getAttributeManager()?.getAttributes() ?? {};
        const v = (a.instanceFillColors ?? a.fillColors)?.value;
        let on = 0;
        const n = v ? Math.floor(v.length / 4) : 0;
        for (let i = 0; i < n; i++) if (v![i * 4 + 3] > 0) on++;
        return [l.id, on, n] as [string, number, number];
      }),
  pick: (x: number, y: number) => {
    const info = deckHandle?.pickObject({ x, y, radius: 1 });
    return info ? { layer: info.layer?.id, sourceLayer: info.sourceLayer?.id, index: info.index, cellKey: cellKeyFromPick(info) } : null;
  },
};

interface DebugLayer {
  id: string;
  getAttributeManager: () => { getAttributes: () => Record<string, { value?: ArrayLike<number> | null } | undefined> } | null;
}

/** Whether the dark theme is in effect (explicit choice, or the system's). */
function useDark(): boolean {
  const theme = useStore((s) => s.theme);
  const [sysDark, setSysDark] = useState(() => !!systemDarkQuery()?.matches);
  useEffect(() => {
    const m = systemDarkQuery();
    if (!m) return;
    const h = () => setSysDark(m.matches);
    m.addEventListener("change", h);
    return () => m.removeEventListener("change", h);
  }, []);
  return theme === "dark" || (theme === "system" && sysDark);
}

export function Viewer() {
  const s = useStore((x) => x);
  const dark = useDark();
  const panels = s.panels;
  const [cols, rows] = LAYOUT[panels.length] ?? [1, 1];
  const container = useRef<HTMLDivElement>(null);
  const size = useRef({ w: 800, h: 600 });
  // vsRef is the source of truth for cameras (updated synchronously); the state copy
  // only drives rendering. Never copy state back into the ref during render: a
  // store-triggered render can run before a queued camera update is applied.
  const [viewStates, setViewStates] = useState<Record<string, VS>>({});
  const vsRef = useRef<Record<string, VS>>({});
  const [modes, setModes] = useState<Record<number, { cell: CellMode; tx: TxMode }>>({});
  const modesRef = useRef(modes);
  modesRef.current = modes;
  /** >1 while an image export renders at a higher pixel density */
  const [exportScale, setExportScale] = useState(1);
  const [glError, setGlError] = useState<string>();
  const [layerError, setLayerError] = useState<string>();
  const hasRendered = useRef(false);

  const measure = () => {
    const el = container.current;
    if (el && el.clientWidth > 0 && el.clientHeight > 0) size.current = { w: el.clientWidth, h: el.clientHeight };
  };
  const panelSize = () => ({ w: size.current.w / cols, h: size.current.h / rows });

  // Level-of-detail per panel, with hysteresis.
  const decide = useCallback(
    (slot: number, vs: VS) => {
      const st = getState();
      const p = st.panels.find((x) => x.slot === slot);
      const mm = p?.manifest;
      if (!p || !mm) return;
      const scale = 2 ** vs.zoom;
      const { w, h } = panelSize();
      const hw = w / 2 / scale, hh = h / 2 / scale;
      const b = [vs.target[0] - hw, vs.target[1] - hh, vs.target[0] + hw, vs.target[1] + hh];
      const ts = mm.grid.tile_size_um;
      const clip = mm.bounds_um;
      const count = (margin: number) => {
        const xa = Math.floor(Math.max(b[0] - margin, clip[0] - margin) / ts);
        const xb = Math.floor(Math.min(b[2] + margin, clip[2] + margin) / ts);
        const ya = Math.floor(Math.max(b[1] - margin, clip[1] - margin) / ts);
        const yb = Math.floor(Math.min(b[3] + margin, clip[3] + margin) / ts);
        return { xa, xb, ya, yb, n: Math.max(0, xb - xa + 1) * Math.max(0, yb - ya + 1) };
      };
      const cellTiles = count(mm.cells?.boundaries?.query_margin_um ?? 0).n;
      const { xa: x0, xb: x1, ya: y0, yb: y1, n: nTiles } = count(0);
      const genes = panelGenes(p, st.genes).map((g) => g.id);
      let est = 0;
      let known = genes.length > 0;
      if (nTiles <= 400) {
        for (const g of genes) {
          const counts = p.geneTileCounts.get(g);
          if (!counts) {
            known = false;
            continue;
          }
          for (let ty = y0; ty <= y1; ty++) for (let tx = x0; tx <= x1; tx++) est += counts.get(tileKey(tx, ty)) ?? 0;
        }
      } else known = false;
      setModes((cur) => {
        const prev = cur[slot] ?? { cell: "centroids", tx: "density" };
        const cell: CellMode = prev.cell === "polygons" ? (cellTiles > 120 ? "centroids" : "polygons") : cellTiles <= 90 ? "polygons" : "centroids";
        const forced = st.transcripts.mode;
        let tx: TxMode;
        if (!genes.length) tx = "off";
        else if (forced !== "auto") tx = forced;
        else if (!known) tx = "density";
        else if (prev.tx === "points") tx = est > POINT_BUDGET * 1.3 || nTiles > 160 ? "density" : "points";
        else tx = est <= POINT_BUDGET && nTiles <= 120 ? "points" : "density";
        if (activePanel(st).slot === slot) {
          setStatus({
            umPerPx: 1 / scale,
            lodCells: cell === "polygons" ? "polygons" : "centroids (zoom in for outlines)",
            lodTx:
              tx === "off"
                ? "no genes selected"
                : tx === "points"
                  ? `points (${known ? est.toLocaleString() : "?"} in view)`
                  : `aggregated density${known ? ` (${est.toLocaleString()} in view)` : ""} – zoom in for points`,
          });
        }
        if (prev.cell === cell && prev.tx === tx && cur[slot]) return cur;
        return { ...cur, [slot]: { cell, tx } };
      });
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [cols, rows],
  );

  const applyViewStates = useCallback(
    (raw: Record<string, VS>) => {
      // deck.gl also keeps a view's zoom as zoomX/zoomY and prefers those when present.
      // A linked or programmatic zoom only sets `zoom`, so stale zoomX/zoomY left over
      // from the view's own last interaction would make its next wheel step start from
      // the old zoom (a no-op or a jump). Store `zoom` only.
      const next: Record<string, VS> = {};
      for (const [k, v] of Object.entries(raw)) {
        const clean = { ...v } as VS & { zoomX?: number; zoomY?: number };
        delete clean.zoomX;
        delete clean.zoomY;
        next[k] = clean;
      }
      setViewStates(next);
      vsRef.current = next;
      cameras = next;
      for (const p of getState().panels) {
        const vs = next[viewId(p.slot)];
        if (vs) decide(p.slot, vs);
      }
    },
    [decide],
  );

  /** Zoom that fits a section's bounds into one panel. */
  const fitZoom = (b: number[]) => {
    const { w, h } = panelSize();
    return Math.log2(Math.min(w / Math.max(1, b[2] - b[0]), h / Math.max(1, b[3] - b[1])) * 0.92);
  };
  const centred = (b: number[], zoom: number, own: number): VS => ({
    target: [(b[0] + b[2]) / 2, (b[1] + b[3]) / 2, 0],
    zoom,
    minZoom: Math.min(own, zoom) - 3,
    maxZoom: 7,
  });

  // Cameras follow the data automatically until the user moves one (or flies to a
  // cell). After that, loading a section only places that panel, so the other
  // panels keep their view and linked zoom stays at the user's zoom. Fit resets this.
  const userMoved = useRef(false);

  /** Fit every loaded panel; with linked zoom all panels use the smallest fit zoom. */
  const fit = useCallback(() => {
    measure();
    userMoved.current = false;
    const st = getState();
    const fits = st.panels.filter((p) => p.manifest).map((p) => ({ p, b: p.manifest!.bounds_um, zoom: fitZoom(p.manifest!.bounds_um) }));
    if (!fits.length) return;
    const common = Math.min(...fits.map((f) => f.zoom));
    const next: Record<string, VS> = { ...vsRef.current };
    for (const { p, b, zoom } of fits) next[viewId(p.slot)] = centred(b, st.linkZoom ? common : zoom, st.linkZoom ? common : zoom);
    applyViewStates(next);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [applyViewStates, cols, rows]);

  /**
   * Centre newly loaded sections in their panels without touching the others. With
   * linked zoom they take the zoom of an unchanged panel (the active one if possible).
   */
  const place = (slots: number[]) => {
    measure();
    const st = getState();
    const next: Record<string, VS> = { ...vsRef.current };
    const others = st.panels.filter((p) => p.manifest && !slots.includes(p.slot) && next[viewId(p.slot)]);
    const ref = others.find((p) => p.slot === activePanel(st).slot) ?? others[0];
    const fits = slots.map((slot) => {
      const b = st.panels.find((p) => p.slot === slot)!.manifest!.bounds_um;
      return { slot, b, own: fitZoom(b) };
    });
    let shared: number | undefined;
    if (st.linkZoom) shared = ref ? next[viewId(ref.slot)].zoom : Math.min(...fits.map((f) => f.own));
    for (const { slot, b, own } of fits) {
      next[viewId(slot)] = centred(b, shared ?? own, own);
      if (st.linkZoom && ref) next[viewId(slot)].minZoom = Math.min(next[viewId(slot)].minZoom!, next[viewId(ref.slot)].minZoom ?? Infinity);
    }
    applyViewStates(next);
  };

  viewerApi.fit = fit;
  viewerApi.flyTo = (x, y, zoom) => {
    const st = getState();
    const id = viewId(activePanel(st).slot);
    const cur = vsRef.current[id];
    if (!cur) return;
    userMoved.current = true;
    const z = zoom ?? Math.max(cur.zoom, 3);
    const next: Record<string, VS> = { ...vsRef.current, [id]: { ...cur, target: [x, y, 0], zoom: z } };
    if (st.linkZoom) for (const k of Object.keys(next)) next[k] = { ...next[k], zoom: z };
    applyViewStates(next);
  };

  // When sections load or the layout changes: fit everything until the user has moved
  // a camera; after that, place only the panels whose section changed. A single panel
  // is always refitted (there is no other zoom to keep).
  const placed = useRef(new Map<number, string>());
  const loadedKey = panels.map((p) => `${p.slot}:${p.manifest?.section_id ?? ""}`).join(",");
  useEffect(() => {
    const loaded = getState().panels.filter((p) => p.manifest);
    const changed = loaded.filter((p) => placed.current.get(p.slot) !== p.manifest!.section_id || !vsRef.current[viewId(p.slot)]);
    placed.current = new Map(loaded.map((p) => [p.slot, p.manifest!.section_id]));
    if (!loaded.length) return;
    if (!userMoved.current || panels.length === 1) fit();
    else if (changed.length) place(changed.map((p) => p.slot));
    else applyViewStates(vsRef.current); // layout change: same cameras, new panel sizes
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [loadedKey, cols, rows]);

  // Turning "link zoom" on brings every panel to the active panel's zoom right away
  // (each keeps its own centre); while linked, all panels share one zoom range so a
  // small and a large section cannot hit different limits.
  const linkZoom = s.linkZoom;
  const wasLinked = useRef(linkZoom);
  useEffect(() => {
    if (linkZoom && !wasLinked.current) {
      const cur = vsRef.current;
      const ref = cur[viewId(activePanel(getState()).slot)] ?? Object.values(cur)[0];
      if (ref) {
        const minZoom = Math.min(...Object.values(cur).map((v) => v.minZoom ?? v.zoom));
        const next: Record<string, VS> = {};
        for (const [k, v] of Object.entries(cur)) next[k] = { ...v, zoom: ref.zoom, minZoom, maxZoom: 7 };
        applyViewStates(next);
      }
    }
    wasLinked.current = linkZoom;
  }, [linkZoom, applyViewStates]);

  // Re-evaluate LOD when genes, tile counts or transcript mode change.
  const lodKey = `${s.genes.map((g) => g.name).join(",")}|${s.transcripts.mode}|${panels.map((p) => p.geneTileCounts.size).join(",")}|${s.active}`;
  useEffect(() => applyViewStates(vsRef.current), [lodKey, applyViewStates]);

  useEffect(() => {
    const h = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement | null;
      if (t && (t.tagName === "INPUT" || t.tagName === "SELECT" || t.tagName === "TEXTAREA" || t.isContentEditable)) return;
      if (e.ctrlKey || e.metaKey || e.altKey) return;
      if (e.key === "f" || e.key === "F") fit();
      if (e.key === "[") setSidePanel("left", !getState().sidePanels.left);
      if (e.key === "]") setSidePanel("right", !getState().sidePanels.right);
      if (e.key === "l" || e.key === "L") setState({ lasso: !getState().lasso });
      if (e.key === "Escape") {
        if (lassoRef.current) {
          lassoRef.current = null;
          setLassoPts(null);
        } else if (getState().lasso) setState({ lasso: false });
        else setPanel(activePanel().slot, { selected: undefined, details: undefined });
      }
    };
    window.addEventListener("keydown", h);
    return () => window.removeEventListener("keydown", h);
  }, [fit]);

  // Lasso: while on, dragging draws an outline (screen px, relative to the viewer) in
  // the panel where it started; on release the cells inside are selected.
  const lasso = s.lasso;
  const lassoRef = useRef<{ pts: [number, number][]; mode: "replace" | "add" | "remove"; pointer: number } | null>(null);
  const [lassoPts, setLassoPts] = useState<[number, number][] | null>(null);
  const local = (e: React.PointerEvent): [number, number] => {
    const r = container.current!.getBoundingClientRect();
    return [e.clientX - r.left, e.clientY - r.top];
  };
  const lassoDown = (e: React.PointerEvent) => {
    if (!lasso || e.button !== 0 || (e.target as HTMLElement).tagName !== "CANVAS") return;
    const mode = e.shiftKey ? "add" : e.altKey ? "remove" : "replace";
    lassoRef.current = { pts: [local(e)], mode, pointer: e.pointerId };
    container.current!.setPointerCapture(e.pointerId);
    setLassoPts(lassoRef.current.pts);
  };
  const lassoMove = (e: React.PointerEvent) => {
    const l = lassoRef.current;
    if (!l || e.pointerId !== l.pointer) return;
    const p = local(e);
    const last = l.pts[l.pts.length - 1];
    if (Math.hypot(p[0] - last[0], p[1] - last[1]) < 3) return;
    l.pts = [...l.pts, p];
    setLassoPts(l.pts);
  };
  const lassoUp = (e: React.PointerEvent) => {
    const l = lassoRef.current;
    if (!l || e.pointerId !== l.pointer) return;
    lassoRef.current = null;
    setLassoPts(null);
    if (l.pts.length < 3) return;
    const r = container.current!.getBoundingClientRect();
    const [sx, sy] = l.pts[0];
    const idx = Math.min(rows - 1, Math.floor(sy / (r.height / rows))) * cols + Math.min(cols - 1, Math.floor(sx / (r.width / cols)));
    const st = getState();
    const panel = st.panels[idx];
    const vp = panel && deckHandle?.deck?.getViewports().find((v) => v.id === viewId(panel.slot));
    if (!panel?.centroids || !vp) return;
    const poly = l.pts.map(([x, y]) => vp.unproject([x - vp.x, y - vp.y]).slice(0, 2) as [number, number]);
    const keys = cellsInPolygon(panel.centroids, panel.cellColors, poly);
    if (idx !== st.active) setState({ active: idx });
    setPanel(panel.slot, (p) => ({ selection: applyLasso(p.selection, keys, l.mode) }));
  };

  const views = useMemo(
    () =>
      panels.map(
        (p, i) =>
          new OrthographicView({
            id: viewId(p.slot),
            x: `${((i % cols) * 100) / cols}%`,
            y: `${(Math.floor(i / cols) * 100) / rows}%`,
            width: `${100 / cols}%`,
            height: `${100 / rows}%`,
            flipY: true,
            controller: {
              dragRotate: false,
              dragPan: !lasso,
              doubleClickZoom: !lasso,
              keyboard: true,
              inertia: false,
              scrollZoom: { speed: 0.04, smooth: false },
            },
          }),
      ),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [panels.map((p) => p.slot).join(","), cols, rows, lasso],
  );

  // Selection versions per slot (renderVersion needs a value that changes with the set).
  const selRef = useRef(new Map<number, { sel: Set<number>; selected?: number; v: number }>());
  const selVersion = (p: PanelData) => {
    const r = selRef.current.get(p.slot);
    if (r && r.sel === p.selection && r.selected === p.selected) return r.v;
    const v = (r?.v ?? 0) + 1;
    selRef.current.set(p.slot, { sel: p.selection, selected: p.selected, v });
    return v;
  };

  const activeSlot = activePanel(s).slot;
  const statusCells = useCallback(
    (t: TileStatus) =>
      setStatus({ tilesCells: `${t.loaded}/${t.visible} chunks${t.inflight ? `, ${t.inflight} loading` : ""}${t.errors ? `, ${t.errors} errors` : ""}` }),
    [],
  );
  const statusTx = useCallback(
    (t: TileStatus) =>
      setStatus({
        tilesTx: t.tooMany ? "too many tiles for points" : `${t.loaded}/${t.visible} tiles${t.inflight ? `, ${t.inflight} loading` : ""}${t.errors ? `, ${t.errors} errors` : ""}`,
      }),
    [],
  );
  const noop = useCallback(() => {}, []);

  const layers = useMemo(() => {
    const out: Layer[] = [];
    for (const p of panels) {
      const m = p.manifest;
      // A manifest of another section (mid-switch) must never shape this section's layers.
      if (!m || !p.api || m.section_id !== p.api.sectionId) continue;
      const prefix = viewId(p.slot);
      const mode = modes[p.slot] ?? { cell: "centroids" as CellMode, tx: "density" as TxMode };
      const ctx: LayerCtx = {
        prefix,
        api: p.api,
        m,
        centroids: p.centroids,
        cellColors: p.cellColors,
        colorVersion: p.colorVersion,
        overlayColors: p.overlayColors,
        overlay: s.overlay,
        selection: p.selection,
        selected: p.selected,
        colorBy: s.colorBy,
        cells: s.cells,
        transcripts: s.transcripts,
        genes: panelGenes(p, s.genes),
        geneTileCounts: p.geneTileCounts,
        gridImage: p.gridImage,
        grid: s.grid,
        ucellImage: p.ucellImage,
        ucellHeat: s.ucell.heat,
        dark,
      };
      // H&E: the first image that is not a fluorescence channel.
      const img = m.images.find((i) => !i.channel);
      const boost = Math.log2(exportScale);
      if (img) out.push(imageLayer(prefix, p.api, img, s.he.visible, s.he.opacity, boost));
      const morph = morphologyLayer(
        prefix,
        p.api,
        m.images.filter((i) => i.channel).map((i) => ({ img: i, set: morphSettings(s.morph, i) })).filter((c) => c.set.on),
        s.morphOpacity,
        boost,
      );
      if (morph) out.push(morph);
      const g = gridLayer(ctx);
      if (g) out.push(g);
      const heat = ucellHeatLayer(ctx);
      if (heat) out.push(heat);
      const isActive = p.slot === activeSlot;
      out.push(...cellLayers({ s: ctx, m, mode: mode.cell, onStatus: isActive ? statusCells : noop, selectionVersion: selVersion(p) }));
      out.push(...transcriptLayers({ s: ctx, m, mode: mode.tx, onStatus: isActive ? statusTx : noop }));
    }
    return out;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [panels, modes, dark, s.morph, s.morphOpacity, s.colorBy, s.overlay, s.ucell.heat, s.cells, s.transcripts, s.genes, s.grid, s.he, activeSlot, statusCells, statusTx, noop, exportScale]);

  viewerApi.capture = async (scale, onProgress) => {
    const deck = deckHandle?.deck;
    const canvas = deck?.getCanvas();
    if (!deck || !canvas) throw new Error("the viewer is not ready");
    const dpr = window.devicePixelRatio || 1;
    const cssW = canvas.clientWidth, cssH = canvas.clientHeight;
    const s2 = Math.max(1, Math.min(scale, MAX_EXPORT_SIDE / (Math.max(cssW, cssH) * dpr)));
    const ratio = dpr * s2;
    const want = [Math.round(cssW * ratio), Math.round(cssH * ratio)];
    setExportScale(s2);
    try {
      const t0 = performance.now();
      for (;;) {
        await new Promise((r) => setTimeout(r, 150));
        const all = (deck as unknown as { layerManager?: { getLayers: () => { isLoaded: boolean }[] } }).layerManager?.getLayers() ?? [];
        const done = all.filter((l) => l.isLoaded).length;
        const sized = Math.abs(canvas.width - want[0]) <= 2 && Math.abs(canvas.height - want[1]) <= 2;
        if (sized && done === all.length) break;
        if (performance.now() - t0 > 120_000) throw new Error("the tiles did not finish loading within 2 minutes");
        onProgress?.(`Loading tiles at ${s2.toFixed(s2 % 1 ? 1 : 0)}× (${done} of ${all.length} layers ready)…`);
      }
      // Drawn and copied in one task: the WebGL buffer still holds this frame.
      deck.redraw("image export");
      const copy = document.createElement("canvas");
      copy.width = canvas.width;
      copy.height = canvas.height;
      copy.getContext("2d")!.drawImage(canvas, 0, 0);
      const st = getState();
      const w = copy.width / cols, h = copy.height / rows;
      return {
        canvas: copy,
        ratio,
        panels: st.panels.map((p, i) => {
          const vs = vsRef.current[viewId(p.slot)];
          const rect: [number, number, number, number] = [Math.round((i % cols) * w), Math.round(Math.floor(i / cols) * h), Math.round(w), Math.round(h)];
          return { slot: p.slot, sectionId: p.sectionId, rect, umPerPx: vs ? 1 / (2 ** vs.zoom * ratio) : NaN };
        }),
      };
    } finally {
      setExportScale(1);
    }
  };

  const slotOfView = (id?: string) => (id?.startsWith("p") ? Number(id.slice(1)) : undefined);

  const onClick = useCallback((info: PickingInfo, ev: { srcEvent?: { shiftKey?: boolean; metaKey?: boolean; ctrlKey?: boolean } }) => {
    const slot = slotOfView(info.viewport?.id);
    if (slot === undefined) return;
    const st = getState();
    const idx = st.panels.findIndex((p) => p.slot === slot);
    if (idx >= 0 && idx !== st.active) setState({ active: idx });
    let key = cellKeyFromPick(info);
    if (!key && info.coordinate) key = unpickedCell(slot, info.coordinate, modesRef.current[slot]?.cell ?? "centroids");
    const multi = ev?.srcEvent?.shiftKey || ev?.srcEvent?.metaKey || ev?.srcEvent?.ctrlKey;
    if (!key) {
      if (!multi) setPanel(slot, { selected: undefined, details: undefined });
      return;
    }
    selectCell(key, !!multi, slot);
  }, []);

  const frames = useRef({ n: 0, t: performance.now() });
  return (
    <div
      className={`viewer${lasso ? " lasso-on" : ""}`}
      ref={container}
      aria-label="Spatial viewer canvas"
      onPointerDown={lassoDown}
      onPointerMove={lassoMove}
      onPointerUp={lassoUp}
      onPointerCancel={() => {
        lassoRef.current = null;
        setLassoPts(null);
      }}
    >
      {lassoPts && (
        <svg className="lasso-svg" aria-hidden="true">
          <polygon points={lassoPts.map((p) => p.join(",")).join(" ")} />
        </svg>
      )}
      {lasso && !lassoPts && <div className="lasso-hint">Lasso: drag around cells · Shift adds · Alt removes · Esc stops</div>}
      {glError && <div className="overlay error">Could not start WebGL2 rendering in this browser: {glError}</div>}
      {layerError && (
        <div className="render-error" role="alert">
          Rendering error: {layerError}{" "}
          <button className="link" onClick={() => setLayerError(undefined)} aria-label="dismiss">
            ×
          </button>
        </div>
      )}
      <DeckGL
        ref={(r) => {
          deckHandle = r;
        }}
        views={views}
        useDevicePixels={exportScale > 1 ? (window.devicePixelRatio || 1) * exportScale : true}
        viewState={viewStates as never}
        layerFilter={({ layer, viewport }) => layer.id.startsWith(`${viewport.id}:`)}
        layers={layers}
        onResize={() => {
          measure();
          applyViewStates(vsRef.current);
        }}
        onViewStateChange={({ viewId: id, viewState }) => {
          const st = getState();
          const vs = viewState as VS;
          userMoved.current = true;
          const next: Record<string, VS> = { ...vsRef.current, [id]: vs };
          if (st.linkZoom) for (const k of Object.keys(next)) if (k !== id) next[k] = { ...next[k], zoom: vs.zoom };
          applyViewStates(next);
          return viewState;
        }}
        onHover={(info) => setStatus({ cursor: info.coordinate ? [info.coordinate[0], info.coordinate[1]] : undefined })}
        onClick={onClick as never}
        onError={(e) => {
          const msg = String(e?.message ?? e);
          console.error("deck.gl error", e);
          if (hasRendered.current) setLayerError(msg);
          else setGlError(msg);
        }}
        onAfterRender={() => {
          hasRendered.current = true;
          const f = frames.current;
          f.n++;
          const now = performance.now();
          if (now - f.t > 1000) {
            setStatus({ fps: Math.round((f.n * 1000) / (now - f.t)) });
            f.n = 0;
            f.t = now;
          }
        }}
        getTooltip={(info) => tooltip(info)}
        getCursor={({ isDragging, isHovering }) => (isDragging ? "grabbing" : isHovering ? "pointer" : "crosshair")}
      />
      {panels.length > 1 &&
        panels.map((p, i) => (
          <PanelHeader key={p.slot} p={p} index={i} cols={cols} rows={rows} active={i === s.active} />
        ))}
    </div>
  );
}

function PanelHeader({ p, index, cols, rows, active }: { p: PanelData; index: number; cols: number; rows: number; active: boolean }) {
  const sections = useStore((s) => s.sections);
  const n = useStore((s) => s.panels.length);
  const legend = p.legend;
  return (
    <div
      className={`panel-frame${active ? " active" : ""}`}
      style={{ left: `${((index % cols) * 100) / cols}%`, top: `${(Math.floor(index / cols) * 100) / rows}%`, width: `${100 / cols}%`, height: `${100 / rows}%` }}
    >
      <div className="panel-head" onMouseDown={() => setState({ active: index })}>
        <select
          value={p.sectionId ?? ""}
          onChange={(e) => openSection(p.slot, e.target.value)}
          aria-label={`Section for panel ${index + 1}`}
        >
          {sections.map((s) => (
            <option key={s.section_id} value={s.section_id} disabled={!s.version}>
              {sectionLabel(s)}
            </option>
          ))}
        </select>
        {p.loadError && <span className="error small">{p.loadError}</span>}
        {legend?.kind === "none" && legend.label.includes("not available") && <span className="muted small">{legend.label}</span>}
        {n > 1 && (
          <button className="link" title="Close panel" aria-label={`Close panel ${index + 1}`} onClick={() => removePanel(p.slot)}>
            ×
          </button>
        )}
      </div>
    </div>
  );
}

export function removePanel(slot: number) {
  const st = getState();
  if (st.panels.length <= 1) return;
  const panels = st.panels.filter((p) => p.slot !== slot);
  closeSlot(slot);
  setState({ panels, active: Math.min(st.active, panels.length - 1) });
}

// deck.gl reports the top-level (composite) layer in `info.layer` and the layer
// that was actually drawn in `info.sourceLayer`; per-chunk cell keys live on the latter.
function cellKeyFromPick(info: PickingInfo): number | undefined {
  if (!info.picked || info.index < 0) return undefined;
  const src = (info.sourceLayer ?? info.layer) as { id: string; props: unknown } | null;
  if (!src) return undefined;
  const props = src.props as { cellKeys?: Uint32Array; pointsData?: PointsData; isCentroids?: boolean };
  let key: number | undefined;
  if (props.cellKeys) key = props.cellKeys[info.index];
  else if (props.pointsData) key = pointDetails(props.pointsData, info.index).cellKey || undefined;
  else if (props.isCentroids) key = info.index + 1;
  const slot = info.viewport?.id?.startsWith("p") ? Number(info.viewport.id.slice(1)) : undefined;
  const colors = getState().panels.find((p) => p.slot === slot)?.cellColors;
  // Cells in hidden categories (alpha 0) are not selectable.
  if (key && colors && colors[(key - 1) * 4 + 3] === 0) return undefined;
  return key;
}

/**
 * The clicked cell where no cell area is drawn (fill off, or zoomed out without dots):
 * the nearest centroid within about one cell radius. Where cells are drawn, a click
 * outside them still selects nothing.
 */
function unpickedCell(slot: number, at: number[], mode: CellMode): number | undefined {
  const st = getState();
  const p = st.panels.find((x) => x.slot === slot);
  const m = p?.manifest;
  if (!p?.centroids || !m?.cells) return undefined;
  const c = st.cells;
  const outlines = !!m.cells.boundaries;
  // Mirrors cellLayers: which cell shapes are drawn (and pickable) right now.
  const dots = c.centroids === "on" || (c.centroids === "auto" && (mode === "centroids" || !outlines) && (c.fill || !outlines));
  if (dots || (mode === "polygons" && outlines && c.fill)) return undefined;
  return nearestCellKey(p.centroids, at[0], at[1], cellDotRadius(m), p.cellColors);
}

export function selectCell(key: number, toggleMulti = false, slot = activePanel().slot) {
  const p = getState().panels.find((x) => x.slot === slot);
  if (!p) return;
  if (toggleMulti) {
    const sel = new Set(p.selection);
    if (sel.has(key)) sel.delete(key);
    else sel.add(key);
    setPanel(slot, { selection: sel });
  }
  setPanel(slot, { selected: key, detailsError: undefined });
  if (getState().foldLegendOnClick) setFolds({ legend: false, "selected-cell": true });
  const api = p.api;
  api
    // LR values (hundreds of columns, one disk read each) load when the inspector opens them.
    ?.cell(key, undefined, { excludeGroups: LR_GROUPS })
    .then((d) => {
      const cur = getState().panels.find((x) => x.slot === slot);
      if (cur?.selected === key && cur.api === api) setPanel(slot, { details: d });
    })
    .catch((e) => setPanel(slot, { detailsError: String(e) }));
}

function tooltip(info: PickingInfo) {
  if (!info.picked) return null;
  const slot = info.viewport?.id?.startsWith("p") ? Number(info.viewport.id.slice(1)) : undefined;
  const p = getState().panels.find((x) => x.slot === slot);
  const m = p?.manifest;
  const src = info.sourceLayer ?? info.layer;
  if (!src || !m) return null;
  const props = src.props as unknown as { pointsData?: PointsData };
  if (props.pointsData) {
    const d = pointDetails(props.pointsData, info.index);
    const g = m.transcripts?.genes[d.geneId];
    return {
      text: `${g?.name ?? d.geneId} · QV ${d.qv.toFixed(2)} · z ${d.z.toFixed(2)} µm\n${d.cellKey ? `cell #${d.cellKey}` : "unassigned"}${d.overlapsNucleus ? " · in nucleus" : ""}\n(${d.x.toFixed(2)}, ${d.y.toFixed(2)}) µm · ${m.section_id}`,
    };
  }
  const key = cellKeyFromPick(info);
  if (key) return { text: `${m.section_id} · cell #${key}${p.legend?.label ? ` · ${p.legend.label}` : ""}\nclick for details · shift-click to add to selection` };
  return null;
}
