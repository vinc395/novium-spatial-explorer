// Morphology (fluorescence) channels: each channel is an 8-bit gray tile pyramid in
// the Xenium pixel frame. A panel draws one tile layer: for every tile the enabled
// channels are fetched, then mixed additively in their colours. Pixel alpha follows the
// strongest signal, so dark background shows what is underneath (H&E or the canvas),
// and colour is un-premultiplied so the result over black equals the additive sum.

import type { Layer } from "@deck.gl/core";
import { BitmapLayer } from "@deck.gl/layers";
import { TileLayer } from "@deck.gl/geo-layers";
import { apiUrl, type SectionApi } from "../api";
import type { MorphChannel } from "../store";
import type { ImageInfo } from "../types";
import { affineToMatrix4 } from "./layers";

export interface MixChannel {
  id: string;
  rgb: [number, number, number];
  /** gain on 0..1 intensities */
  gain: number;
  opacity: number;
}

/** RGBA pixels mixing `gray[id]` channels (missing channels are skipped). */
export function mixChannels(gray: Map<string, Uint8Array>, n: number, chans: MixChannel[]): Uint8ClampedArray {
  const out = new Uint8ClampedArray(n * 4);
  const use = chans.map((c) => ({ c, px: gray.get(c.id) })).filter((x) => x.px) as { c: MixChannel; px: Uint8Array }[];
  for (let i = 0; i < n; i++) {
    let a = 0, r = 0, g = 0, b = 0;
    for (const { c, px } of use) {
      const v = Math.min(1, (px[i] / 255) * c.gain) * c.opacity;
      if (v <= 0) continue;
      if (v > a) a = v;
      r += c.rgb[0] * v;
      g += c.rgb[1] * v;
      b += c.rgb[2] * v;
    }
    if (a > 0) {
      const o = i * 4;
      out[o] = r / a;
      out[o + 1] = g / a;
      out[o + 2] = b / a;
      out[o + 3] = a * 255;
    }
  }
  return out;
}

export function hexToRgb(hex: string): [number, number, number] {
  const m = /^#?([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(hex);
  return m ? [parseInt(m[1], 16), parseInt(m[2], 16), parseInt(m[3], 16)] : [255, 255, 255];
}

interface MorphTile {
  gray: Map<string, Uint8Array>;
  w: number;
  h: number;
  cache?: { key: string; image: ImageData };
}

/** Fetch one JPEG tile and return its luminance. */
async function loadGray(url: string, signal?: AbortSignal): Promise<{ w: number; h: number; px: Uint8Array } | null> {
  const u = apiUrl(url);
  if (!u) return null;
  const r = await fetch(u, { signal });
  if (!r.ok) return null;
  const bmp = await createImageBitmap(await r.blob());
  const { width: w, height: h } = bmp;
  const canvas = new OffscreenCanvas(w, h);
  const ctx = canvas.getContext("2d", { willReadFrequently: true })!;
  ctx.drawImage(bmp, 0, 0);
  bmp.close();
  const d = ctx.getImageData(0, 0, w, h).data;
  const px = new Uint8Array(w * h);
  for (let i = 0; i < px.length; i++) px[i] = d[i * 4];
  return { w, h, px };
}

/** The composite layer for one panel; null when no channel is switched on. */
export function morphologyLayer(
  prefix: string,
  api: SectionApi,
  chans: { img: ImageInfo; set: MorphChannel }[],
  opacity = 1,
  /** extra levels of detail (log2 of the image-export scale) */
  tileBoost = 0,
): Layer | null {
  if (!chans.length) return null;
  const base = chans[0].img;
  const reg = base.registration;
  const dpr = Math.min(2, window.devicePixelRatio || 1);
  const W = base.width, H = base.height;
  const ids = chans.map((c) => c.img.id);
  const mix: MixChannel[] = chans.map((c) => ({ id: c.img.id, rgb: hexToRgb(c.set.color), gain: c.set.brightness, opacity: c.set.opacity }));
  const key = JSON.stringify(mix);
  return new TileLayer<MorphTile | null>({
    id: `${prefix}:morph-${api.sectionId}@${api.version}`,
    tileSize: base.tile_size,
    minZoom: -(base.levels - 1),
    maxZoom: 0,
    extent: [0, 0, W, H],
    modelMatrix: affineToMatrix4(reg.matrix_xu_from_px) as never,
    zoomOffset: Math.log2(reg.approx_pixel_size_um) + Math.log2(dpr) + tileBoost,
    maxRequests: 12,
    debounceTime: 20,
    refinementStrategy: "best-available",
    // The master opacity applies to the mixed image, so changing it needs no re-mixing.
    opacity,
    updateTriggers: { getTileData: ids.join(","), renderSubLayers: key },
    getTileData: async ({ index, signal }) => {
      const level = -index.z;
      const span = base.tile_size * 2 ** level;
      if (index.x < 0 || index.y < 0 || index.x * span >= W || index.y * span >= H || level < 0 || level >= base.levels) return null;
      const gray = new Map<string, Uint8Array>();
      let w = 0, h = 0;
      await Promise.all(
        ids.map(async (id) => {
          const g = await loadGray(api.imageTileUrl(id, level, index.x, index.y), signal);
          if (g) {
            gray.set(id, g.px);
            [w, h] = [g.w, g.h];
          }
        }),
      );
      return gray.size ? { gray, w, h } : null;
    },
    renderSubLayers: (props) => {
      const t = props.data as MorphTile | null;
      if (!t) return null;
      if (t.cache?.key !== key) t.cache = { key, image: new ImageData(mixChannels(t.gray, t.w * t.h, mix) as Uint8ClampedArray<ArrayBuffer>, t.w, t.h) };
      const { left, top, right, bottom } = props.tile.bbox as { left: number; top: number; right: number; bottom: number };
      return new BitmapLayer({
        id: props.id,
        image: t.cache.image,
        bounds: [left, Math.min(bottom, H), Math.min(right, W), top],
        modelMatrix: props.modelMatrix,
        opacity: props.opacity,
        pickable: false,
      });
    },
  });
}
