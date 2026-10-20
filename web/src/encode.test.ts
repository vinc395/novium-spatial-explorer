import { describe, expect, it } from "vitest";
import { crc32, deflate, encodePdf, encodeTiff, jpegWithDpi, pngWithDpi } from "./export/encode";

function image(w: number, h: number, alpha = 255): ImageData {
  const data = new Uint8ClampedArray(w * h * 4);
  for (let i = 0; i < w * h; i++) data.set([i % 256, (i * 7) % 256, (i * 13) % 256, alpha], 4 * i);
  return { width: w, height: h, data } as unknown as ImageData;
}

async function inflate(z: Uint8Array): Promise<Uint8Array> {
  const s = new Blob([z as BlobPart]).stream().pipeThrough(new DecompressionStream("deflate"));
  return new Uint8Array(await new Response(s).arrayBuffer());
}

function tiffTags(t: Uint8Array) {
  const dv = new DataView(t.buffer, t.byteOffset);
  const ifd = dv.getUint32(4, true);
  const tags = new Map<number, number>();
  for (let i = 0; i < dv.getUint16(ifd, true); i++) {
    const o = ifd + 2 + 12 * i;
    const type = dv.getUint16(o + 2, true), count = dv.getUint32(o + 4, true);
    tags.set(dv.getUint16(o, true), type === 3 && count === 1 ? dv.getUint16(o + 8, true) : dv.getUint32(o + 8, true));
  }
  return { dv, tags };
}

describe("TIFF export", () => {
  it("is a baseline RGB TIFF (Deflate, predictor) that decodes back to the pixels, with its DPI", async () => {
    const src = image(5, 3);
    const t = await encodeTiff(src, 300);
    const { dv, tags } = tiffTags(t);
    expect(String.fromCharCode(t[0], t[1])).toBe("II");
    expect(dv.getUint16(2, true)).toBe(42);
    expect([tags.get(256), tags.get(257), tags.get(259), tags.get(262), tags.get(277), tags.get(317)]).toEqual([5, 3, 8, 2, 3, 2]);
    const x = tags.get(282)!;
    expect(dv.getUint32(x, true) / dv.getUint32(x + 4, true)).toBe(300);
    expect(tags.get(296)).toBe(2);
    const raw = await inflate(t.subarray(tags.get(273)!, tags.get(273)! + tags.get(279)!));
    for (let y = 0; y < 3; y++) for (let i = 3; i < 15; i++) raw[y * 15 + i] = (raw[y * 15 + i] + raw[y * 15 + i - 3]) & 255;
    for (let p = 0; p < 15; p++) expect([...raw.subarray(3 * p, 3 * p + 3)]).toEqual([...src.data.subarray(4 * p, 4 * p + 3)]);
  });

  it("keeps the alpha channel when asked (transparent background)", async () => {
    const { tags } = tiffTags(await encodeTiff(image(2, 2, 0), 72, true));
    expect([tags.get(277), tags.get(338)]).toEqual([4, 2]);
  });
});

describe("PNG and JPEG DPI", () => {
  const ascii = (s: string) => [...s].map((c) => c.charCodeAt(0));
  const chunk = (type: string, data: number[]) => {
    const td = Uint8Array.from([...ascii(type), ...data]);
    const head = new Uint8Array(4), tail = new Uint8Array(4);
    new DataView(head.buffer).setUint32(0, data.length);
    new DataView(tail.buffer).setUint32(0, crc32(td));
    return [...head, ...td, ...tail];
  };

  it("adds one pHYs chunk after IHDR", async () => {
    const idat = await deflate(Uint8Array.from([0, 255, 0, 0, 255]));
    const png = Uint8Array.from([137, 80, 78, 71, 13, 10, 26, 10, ...chunk("IHDR", [0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0]), ...chunk("IDAT", [...idat]), ...chunk("IEND", [])]);
    const out = pngWithDpi(png, 300);
    expect(out.length).toBe(png.length + 21);
    expect(String.fromCharCode(...out.subarray(37, 41))).toBe("pHYs");
    const dv = new DataView(out.buffer);
    expect([dv.getUint32(41), dv.getUint32(45), out[49]]).toEqual([11811, 11811, 1]);
    expect(dv.getUint32(50)).toBe(crc32(out.subarray(37, 50)));
    expect(pngWithDpi(out, 600)).toBe(out);
  });

  it("writes the density into the JFIF header", () => {
    const jpg = Uint8Array.from([0xff, 0xd8, 0xff, 0xe0, 0, 16, ...ascii("JFIF"), 0, 1, 1, 0, 0, 1, 0, 1, 0, 0, 0xff, 0xd9]);
    const out = jpegWithDpi(jpg, 300);
    expect([...out.subarray(13, 18)]).toEqual([1, 1, 44, 1, 44]);
    expect(jpegWithDpi(Uint8Array.from([0xff, 0xd8, 0xff, 0xdb, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]), 300)[13]).toBe(0);
  });
});

describe("PDF export", () => {
  it("is one page of the image's size at its DPI, with a valid cross-reference table", async () => {
    const pdf = await encodePdf(image(144, 72), 144, true);
    const s = new TextDecoder("latin1").decode(pdf);
    expect(s.startsWith("%PDF-1.4")).toBe(true);
    expect(s).toContain("/MediaBox [0 0 72.00 36.00]");
    expect(s).toContain("/SMask 6 0 R");
    const start = Number(/startxref\n(\d+)/.exec(s)![1]);
    expect(s.slice(start, start + 4)).toBe("xref");
    const offsets = [...s.slice(start).matchAll(/(\d{10}) 00000 n/g)].map((m) => Number(m[1]));
    expect(offsets.length).toBe(6);
    offsets.forEach((o, i) => expect(s.slice(o, o + `${i + 1} 0 obj`.length)).toBe(`${i + 1} 0 obj`));
  });
});
