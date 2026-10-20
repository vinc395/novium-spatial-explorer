// Decoders for the versioned little-endian binary formats defined in
// crates/xenium-core/src/codec.rs. All decoders validate exact lengths and
// return typed-array views into the original buffer (no copies).

export const VERSION = 1;
export const POINT_RECORD_BYTES = 16;
export const QV_SCALE = 4;
export const POINT_FLAG_OVERLAPS_NUCLEUS = 1;

export class CodecError extends Error {}

function header(buf: ArrayBuffer, magic: string, minLen: number): DataView {
  if (buf.byteLength < minLen) throw new CodecError(`${magic}: truncated header`);
  const dv = new DataView(buf);
  const m = String.fromCharCode(dv.getUint8(0), dv.getUint8(1), dv.getUint8(2), dv.getUint8(3));
  if (m !== magic) throw new CodecError(`expected ${magic}, got ${JSON.stringify(m)}`);
  const v = dv.getUint16(4, true);
  if (v !== VERSION) throw new CodecError(`${magic}: unsupported version ${v}`);
  return dv;
}

function expectLen(buf: ArrayBuffer, magic: string, n: number) {
  if (buf.byteLength !== n) throw new CodecError(`${magic}: expected ${n} bytes, got ${buf.byteLength}`);
}

export interface PointsTile {
  flags: number;
  count: number;
  /** gene id per segment and its record count, in order */
  segments: { geneId: number; count: number }[];
  /** the raw 16-byte records (x f32, y f32, cell_key u32, z i16, qv u8, flags u8) */
  records: DataView;
  /** x,y interleaved view with stride 16 bytes (use as a deck.gl binary attribute) */
  buffer: ArrayBuffer;
  recordOffset: number;
}

export function decodePoints(buf: ArrayBuffer): PointsTile {
  const dv = header(buf, "XPT1", 16);
  const flags = dv.getUint16(6, true);
  const n = dv.getUint32(8, true);
  const s = dv.getUint32(12, true);
  const recordOffset = 16 + 8 * s;
  expectLen(buf, "XPT1", recordOffset + POINT_RECORD_BYTES * n);
  const segments = [];
  let total = 0;
  for (let i = 0; i < s; i++) {
    const geneId = dv.getUint32(16 + 8 * i, true);
    const count = dv.getUint32(20 + 8 * i, true);
    total += count;
    segments.push({ geneId, count });
  }
  if (total !== n) throw new CodecError("XPT1: segment counts do not sum to record_count");
  return { flags, count: n, segments, records: new DataView(buf, recordOffset), buffer: buf, recordOffset };
}

/** An XPT1 tile with only the records of `genes` (the online demo stores every gene per tile). */
export function selectPointGenes(buf: ArrayBuffer, genes: number[]): ArrayBuffer {
  const t = decodePoints(buf);
  const want = new Set(genes);
  const keep: { geneId: number; count: number; from: number }[] = [];
  let at = 0;
  for (const seg of t.segments) {
    if (want.has(seg.geneId)) keep.push({ ...seg, from: at });
    at += seg.count;
  }
  const n = keep.reduce((a, k) => a + k.count, 0);
  const out = new ArrayBuffer(16 + 8 * keep.length + POINT_RECORD_BYTES * n);
  const src = new Uint8Array(buf), dst = new Uint8Array(out), dv = new DataView(out);
  dst.set(src.subarray(0, 8)); // magic, version, flags
  dv.setUint32(8, n, true);
  dv.setUint32(12, keep.length, true);
  let o = 16 + 8 * keep.length;
  keep.forEach((k, i) => {
    dv.setUint32(16 + 8 * i, k.geneId, true);
    dv.setUint32(20 + 8 * i, k.count, true);
    const a = t.recordOffset + POINT_RECORD_BYTES * k.from;
    dst.set(src.subarray(a, a + POINT_RECORD_BYTES * k.count), o);
    o += POINT_RECORD_BYTES * k.count;
  });
  return out;
}

export interface PolygonChunk {
  kind: number;
  cellKeys: Uint32Array;
  /** n+1 vertex offsets */
  starts: Uint32Array;
  /** x,y interleaved, µm */
  xy: Float32Array;
}

export function decodePolygons(buf: ArrayBuffer): PolygonChunk {
  const dv = header(buf, "XCC1", 16);
  const kind = dv.getUint16(6, true);
  const n = dv.getUint32(8, true);
  const v = dv.getUint32(12, true);
  expectLen(buf, "XCC1", 16 + 4 * n + 4 * (n + 1) + 8 * v);
  const cellKeys = new Uint32Array(buf, 16, n);
  const starts = new Uint32Array(buf, 16 + 4 * n, n + 1);
  const xy = new Float32Array(buf, 16 + 8 * n + 4, 2 * v);
  if (starts[0] !== 0 || starts[n] !== v) throw new CodecError("XCC1: bad start indices");
  for (let i = 0; i < n; i++) if (starts[i] > starts[i + 1]) throw new CodecError("XCC1: non-monotonic starts");
  return { kind, cellKeys, starts, xy };
}

export function decodeCentroids(buf: ArrayBuffer): Float32Array {
  const dv = header(buf, "XCE1", 16);
  const n = dv.getUint32(8, true);
  expectLen(buf, "XCE1", 16 + 8 * n);
  return new Float32Array(buf, 16, 2 * n);
}

export type FieldColumn =
  | { kind: "numeric"; values: Float32Array }
  | { kind: "categorical"; codes: Uint32Array; nCategories: number };

export function decodeField(buf: ArrayBuffer): FieldColumn {
  const dv = header(buf, "XFD1", 16);
  const kind = dv.getUint16(6, true);
  const n = dv.getUint32(8, true);
  const ncat = dv.getUint32(12, true);
  expectLen(buf, "XFD1", 16 + 4 * n);
  if (kind === 0) return { kind: "numeric", values: new Float32Array(buf, 16, n) };
  if (kind === 1) {
    const codes = new Uint32Array(buf, 16, n);
    for (let i = 0; i < n; i++) if (codes[i] > ncat) throw new CodecError("XFD1: category code out of range");
    return { kind: "categorical", codes, nCategories: ncat };
  }
  throw new CodecError(`XFD1: unknown kind ${kind}`);
}

export interface DensityTile {
  level: number;
  width: number;
  height: number;
  maxCount: number;
  total: number;
  counts: Uint32Array;
}

export function decodeDensity(buf: ArrayBuffer): DensityTile {
  const dv = header(buf, "XDT1", 24);
  const level = dv.getUint16(6, true);
  const width = dv.getUint32(8, true);
  const height = dv.getUint32(12, true);
  expectLen(buf, "XDT1", 24 + 4 * width * height);
  return {
    level,
    width,
    height,
    maxCount: dv.getUint32(16, true),
    total: dv.getUint32(20, true),
    counts: new Uint32Array(buf, 24, width * height),
  };
}

const BIAS = 32768;
export function tileKey(tx: number, ty: number): number {
  return (((ty + BIAS) << 16) | ((tx + BIAS) & 0xffff)) >>> 0;
}
export function tileFromKey(key: number): [number, number] {
  return [(key & 0xffff) - BIAS, (key >>> 16) - BIAS];
}

/** Map tile key -> transcript count for one gene. */
export function decodeGeneTiles(buf: ArrayBuffer): { geneId: number; counts: Map<number, number> } {
  const dv = header(buf, "XGT1", 16);
  const n = dv.getUint32(8, true);
  const geneId = dv.getUint32(12, true);
  expectLen(buf, "XGT1", 16 + 8 * n);
  const counts = new Map<number, number>();
  for (let i = 0; i < n; i++) counts.set(dv.getUint32(16 + 8 * i, true), dv.getUint32(20 + 8 * i, true));
  return { geneId, counts };
}

export interface GridRaster {
  width: number;
  height: number;
  column: FieldColumn;
}

export function decodeGridRaster(buf: ArrayBuffer): GridRaster {
  const dv = header(buf, "XGR1", 20);
  const kind = dv.getUint16(6, true);
  const width = dv.getUint32(8, true);
  const height = dv.getUint32(12, true);
  const ncat = dv.getUint32(16, true);
  expectLen(buf, "XGR1", 20 + 4 * width * height);
  const column: FieldColumn =
    kind === 0
      ? { kind: "numeric", values: new Float32Array(buf, 20, width * height) }
      : { kind: "categorical", codes: new Uint32Array(buf, 20, width * height), nCategories: ncat };
  return { width, height, column };
}
