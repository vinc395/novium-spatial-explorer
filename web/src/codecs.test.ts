import { describe, expect, it } from "vitest";
import {
  decodeCentroids,
  decodeDensity,
  decodeField,
  decodePoints,
  decodePolygons,
  tileFromKey,
  tileKey,
  POINT_RECORD_BYTES,
} from "./codecs";

function bytes(parts: (number[] | ArrayBuffer)[]): ArrayBuffer {
  const arrs = parts.map((p) => (p instanceof ArrayBuffer ? new Uint8Array(p) : Uint8Array.from(p)));
  const out = new Uint8Array(arrs.reduce((n, a) => n + a.length, 0));
  let o = 0;
  for (const a of arrs) {
    out.set(a, o);
    o += a.length;
  }
  return out.buffer;
}
const ascii = (s: string) => Array.from(s, (c) => c.charCodeAt(0));
const u16 = (v: number) => [v & 255, v >> 8];
const u32 = (v: number) => [v & 255, (v >> 8) & 255, (v >> 16) & 255, (v >>> 24) & 255];
const f32 = (v: number) => Array.from(new Uint8Array(new Float32Array([v]).buffer));

describe("XPT1", () => {
  // Golden record shared with crates/xenium-core codec::tests::point_record_golden_bytes
  const golden = [0, 0, 0x80, 0x3f, 0, 0, 0, 0xc0, 4, 3, 2, 1, 0xfe, 0xff, 80, 3];
  const buf = bytes([ascii("XPT1"), u16(1), u16(0), u32(1), u32(1), u32(7), u32(1), golden]);
  it("decodes the golden record", () => {
    const p = decodePoints(buf);
    expect(p.count).toBe(1);
    expect(p.segments).toEqual([{ geneId: 7, count: 1 }]);
    expect(p.records.getFloat32(0, true)).toBe(1);
    expect(p.records.getFloat32(4, true)).toBe(-2);
    expect(p.records.getUint32(8, true)).toBe(0x01020304);
    expect(p.records.getInt16(12, true)).toBe(-2);
    expect(p.records.getUint8(14)).toBe(80);
    expect(p.recordOffset + POINT_RECORD_BYTES).toBe(buf.byteLength);
  });
  it("rejects truncated, wrong version and bad segment sums", () => {
    expect(() => decodePoints(buf.slice(0, buf.byteLength - 1))).toThrow();
    const v2 = bytes([ascii("XPT1"), u16(2), u16(0), u32(0), u32(0)]);
    expect(() => decodePoints(v2)).toThrow(/version/);
    const badSum = bytes([ascii("XPT1"), u16(1), u16(0), u32(1), u32(1), u32(7), u32(2), golden]);
    expect(() => decodePoints(badSum)).toThrow();
    expect(decodePoints(bytes([ascii("XPT1"), u16(1), u16(0), u32(0), u32(0)])).count).toBe(0);
  });
});

describe("XCC1 / XCE1 / XFD1 / XDT1", () => {
  it("polygons", () => {
    const buf = bytes([
      ascii("XCC1"), u16(1), u16(1), u32(1), u32(3),
      u32(5), u32(0), u32(3),
      f32(0), f32(0), f32(1), f32(0), f32(1), f32(1),
    ]);
    const c = decodePolygons(buf);
    expect(c.kind).toBe(1);
    expect(Array.from(c.cellKeys)).toEqual([5]);
    expect(Array.from(c.xy)).toEqual([0, 0, 1, 0, 1, 1]);
    const bad = bytes([ascii("XCC1"), u16(1), u16(0), u32(1), u32(3), u32(5), u32(1), u32(3), new ArrayBuffer(24)]);
    expect(() => decodePolygons(bad)).toThrow();
  });
  it("centroids, fields, density", () => {
    expect(Array.from(decodeCentroids(bytes([ascii("XCE1"), u16(1), u16(0), u32(1), u32(0), f32(2.5), f32(3)])))).toEqual([2.5, 3]);
    const cat = decodeField(bytes([ascii("XFD1"), u16(1), u16(1), u32(2), u32(70), u32(0), u32(70)]));
    expect(cat.kind).toBe("categorical");
    expect(() => decodeField(bytes([ascii("XFD1"), u16(1), u16(1), u32(1), u32(2), u32(3)]))).toThrow();
    const d = decodeDensity(bytes([ascii("XDT1"), u16(1), u16(2), u32(1), u32(2), u32(9), u32(10), u32(1), u32(9)]));
    expect(d.level).toBe(2);
    expect(Array.from(d.counts)).toEqual([1, 9]);
  });
  it("tile keys match Rust packing", () => {
    for (const [x, y] of [[0, 0], [-1, 5], [300, -2]]) expect(tileFromKey(tileKey(x, y))).toEqual([x, y]);
    expect(tileKey(5, 0) < tileKey(0, 1)).toBe(true);
    expect(tileKey(0, 0)).toBe(((32768 << 16) | 32768) >>> 0);
  });
});
