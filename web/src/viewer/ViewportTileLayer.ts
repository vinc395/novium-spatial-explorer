// Generic fixed-grid tile manager for data tiles in Xenium µm.
//
// * Visible tiles = viewport bounds expanded by `margin` (needed because polygons
//   are chunked by their cell centroid), clipped to `extent`.
// * Fetches run with bounded concurrency, nearest-to-centre first, and are
//   aborted as soon as a tile leaves the view or `cacheKey` changes, so stale
//   responses can never be drawn for another section, version or gene set.
// * Loaded tiles live in an LRU; only currently visible tiles are rendered.

import { CompositeLayer, type Layer, type UpdateParameters, type CompositeLayerProps } from "@deck.gl/core";

export interface TileStatus {
  visible: number;
  loaded: number;
  inflight: number;
  errors: number;
  tooMany: boolean;
}

export interface LoadedTile<T> {
  tx: number;
  ty: number;
  key: string;
  data: T;
}

interface Entry<T> {
  tx: number;
  ty: number;
  data?: T;
  error?: unknown;
  ctrl?: AbortController;
  used: number;
  /** Sublayers built for `layersVersion`; reused while it is unchanged so that
   *  panning never re-tessellates or re-uploads loaded tiles. */
  layers?: Layer[];
  layersVersion?: string | number;
}

export type ViewportTileLayerProps<T> = CompositeLayerProps & {
  tileSize: number;
  margin: number;
  extent: [number, number, number, number];
  cacheKey: string;
  fetchTile: (tx: number, ty: number, signal: AbortSignal) => Promise<T>;
  renderTile: (tile: LoadedTile<T>) => Layer | Layer[] | null;
  /** Bumped by the parent whenever renderTile's output would change. */
  renderVersion: string | number;
  maxVisibleTiles: number;
  maxCachedTiles: number;
  maxConcurrent: number;
  onStatus?: (s: TileStatus) => void;
};

type State<T> = {
  /** The newest layer instance; async callbacks must use its props, not their own. */
  owner: ViewportTileLayer<T>;
  finalized: boolean;
  cache: Map<string, Entry<T>>;
  cacheKey: string;
  visible: string[];
  inflight: number;
  queue: string[];
  frame: number;
  tooMany: boolean;
  /** Tile keys whose sublayers were part of the previous render. */
  lastRendered: Set<string>;
  lastStatus: string;
};

export class ViewportTileLayer<T> extends CompositeLayer<ViewportTileLayerProps<T>> {
  static layerName = "ViewportTileLayer";
  static defaultProps = {
    margin: 0,
    maxVisibleTiles: 400,
    maxCachedTiles: 600,
    maxConcurrent: 6,
    renderVersion: 0,
  };
  declare state: State<T>;

  /** Every visible tile fetched (or failed) and drawn: an image export waits for this. */
  get isLoaded(): boolean {
    const st = this.state;
    if (!st || st.inflight > 0 || st.queue.length) return false;
    for (const k of st.visible) {
      const e = st.cache.get(k);
      if (e?.data === undefined && e?.error === undefined) return false;
    }
    return super.isLoaded;
  }

  initializeState() {
    this.setState({ owner: this, finalized: false, cache: new Map(), cacheKey: this.props.cacheKey, visible: [], inflight: 0, queue: [], frame: 0, tooMany: false, lastRendered: new Set(), lastStatus: "" });
  }

  shouldUpdateState({ changeFlags }: UpdateParameters<this>) {
    return changeFlags.somethingChanged;
  }

  updateState({ props, changeFlags }: UpdateParameters<this>) {
    this.state.owner = this;
    if (props.cacheKey !== this.state.cacheKey) {
      this.clear();
      this.state.cacheKey = props.cacheKey;
    }
    if (changeFlags.viewportChanged || changeFlags.propsChanged || changeFlags.updateTriggersChanged) {
      this.updateVisible();
    }
  }

  finalizeState() {
    this.state.finalized = true;
    this.clear();
  }

  private clear() {
    for (const e of this.state.cache.values()) e.ctrl?.abort();
    this.state.cache = new Map();
    this.state.queue = [];
    this.state.inflight = 0;
  }

  private updateVisible() {
    const { tileSize, margin, extent, maxVisibleTiles } = this.props;
    const vp = this.context.viewport;
    const b = vp.getBounds(); // [minX, minY, maxX, maxY] in world units
    const x0 = Math.max(b[0] - margin, extent[0] - margin);
    const y0 = Math.max(b[1] - margin, extent[1] - margin);
    const x1 = Math.min(b[2] + margin, extent[2] + margin);
    const y1 = Math.min(b[3] + margin, extent[3] + margin);
    const st = this.state;
    st.frame++;
    if (x1 < x0 || y1 < y0) {
      st.visible = [];
      st.tooMany = false;
      this.abortInvisible(new Set());
      this.report();
      return;
    }
    const tx0 = Math.floor(x0 / tileSize), tx1 = Math.floor(x1 / tileSize);
    const ty0 = Math.floor(y0 / tileSize), ty1 = Math.floor(y1 / tileSize);
    const n = (tx1 - tx0 + 1) * (ty1 - ty0 + 1);
    st.tooMany = n > maxVisibleTiles;
    if (st.tooMany) {
      st.visible = [];
      this.abortInvisible(new Set());
      this.report();
      return;
    }
    const cx = (b[0] + b[2]) / 2, cy = (b[1] + b[3]) / 2;
    const keys: { k: string; d: number }[] = [];
    for (let ty = ty0; ty <= ty1; ty++) {
      for (let tx = tx0; tx <= tx1; tx++) {
        const k = `${tx},${ty}`;
        const d = ((tx + 0.5) * tileSize - cx) ** 2 + ((ty + 0.5) * tileSize - cy) ** 2;
        keys.push({ k, d });
        let e = st.cache.get(k);
        if (!e) {
          e = { tx, ty, used: st.frame };
          st.cache.set(k, e);
        }
        e.used = st.frame;
      }
    }
    keys.sort((a, b) => a.d - b.d);
    st.visible = keys.map((k) => k.k);
    const vis = new Set(st.visible);
    this.abortInvisible(vis);
    st.queue = st.visible.filter((k) => {
      const e = st.cache.get(k)!;
      return e.data === undefined && e.error === undefined && !e.ctrl;
    });
    this.pump();
    this.evict(vis);
    this.report();
  }

  private abortInvisible(vis: Set<string>) {
    for (const [k, e] of this.state.cache) {
      if (e.ctrl && !vis.has(k)) {
        e.ctrl.abort();
        e.ctrl = undefined;
        this.state.inflight--;
      }
    }
  }

  private evict(vis: Set<string>) {
    const { maxCachedTiles } = this.props;
    const st = this.state;
    if (st.cache.size <= maxCachedTiles) return;
    const old = [...st.cache.entries()].filter(([k, e]) => !vis.has(k) && !e.ctrl).sort((a, b) => a[1].used - b[1].used);
    for (const [k] of old.slice(0, st.cache.size - maxCachedTiles)) st.cache.delete(k);
  }

  pump() {
    const st = this.state;
    while (st.inflight < this.props.maxConcurrent && st.queue.length) {
      const k = st.queue.shift()!;
      const e = st.cache.get(k);
      if (!e || e.data !== undefined || e.ctrl) continue;
      const ctrl = new AbortController();
      e.ctrl = ctrl;
      st.inflight++;
      const cacheKey = st.cacheKey;
      st.owner.props
        .fetchTile(e.tx, e.ty, ctrl.signal)
        .then((data) => {
          if (ctrl.signal.aborted || cacheKey !== st.cacheKey) return;
          e.data = data;
        })
        .catch((err) => {
          if (ctrl.signal.aborted || cacheKey !== st.cacheKey) return;
          e.error = err;
          console.warn(`tile ${k} failed`, err);
        })
        .finally(() => {
          if (ctrl.signal.aborted || cacheKey !== st.cacheKey) return;
          e.ctrl = undefined;
          st.inflight--;
          const owner = st.owner;
          if (st.finalized) return;
          owner.pump();
          owner.report();
          owner.setNeedsUpdate();
        });
    }
  }

  report() {
    const st = this.state;
    let loaded = 0, errors = 0;
    for (const k of st.visible) {
      const e = st.cache.get(k);
      if (e?.data !== undefined) loaded++;
      if (e?.error !== undefined) errors++;
    }
    const s: TileStatus = { visible: st.visible.length, loaded, inflight: st.inflight, errors, tooMany: st.tooMany };
    const sig = JSON.stringify(s);
    if (sig !== st.lastStatus) {
      st.lastStatus = sig;
      st.owner.props.onStatus?.(s);
    }
  }

  renderLayers() {
    const out: Layer[] = [];
    const version = this.props.renderVersion;
    const rendered = new Set<string>();
    for (const k of this.state.visible) {
      const e = this.state.cache.get(k);
      if (e?.data === undefined) continue;
      // deck.gl finalizes a sublayer as soon as it is dropped from a render; such an
      // instance must never be returned again ("finalized layer cannot be reused").
      // Only reuse instances that were rendered last time; otherwise build new ones
      // over the same (stable) data objects.
      if (!this.state.lastRendered.has(k)) e.layers = undefined;
      if (!e.layers || e.layersVersion !== version) {
        const l = this.props.renderTile({ tx: e.tx, ty: e.ty, key: k, data: e.data });
        e.layers = (Array.isArray(l) ? l : l ? [l] : []).map((layer) => layer.clone({ id: `${this.props.id}-${layer.id}` }));
        e.layersVersion = version;
      }
      out.push(...e.layers);
      rendered.add(k);
    }
    this.state.lastRendered = rendered;
    return out;
  }
}
