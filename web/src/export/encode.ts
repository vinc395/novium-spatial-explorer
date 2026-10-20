// File writers for the view export. PNG and JPEG come from the browser's encoder and get
// their DPI written in; TIFF (Deflate) and PDF (one lossless image on a page of the
// matching physical size) are written here. Deflate uses the browser's CompressionStream.

const CRC_TABLE = (() => {
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c >>> 0;
  }
  return t;
})();

export function crc32(bytes: Uint8Array): number {
  let c = 0xffffffff;
  for (let i = 0; i < bytes.length; i++) c = CRC_TABLE[(c ^ bytes[i]) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function concat(parts: Uint8Array[]): Uint8Array {
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let o = 0;
  for (const p of parts) {
    out.set(p, o);
    o += p.length;
  }
  return out;
}

const ascii = (s: string) => new TextEncoder().encode(s);

/** zlib-wrapped Deflate (what TIFF compression 8 and PDF FlateDecode expect). */
export async function deflate(data: Uint8Array): Promise<Uint8Array> {
  const stream = new Blob([data as BlobPart]).stream().pipeThrough(new CompressionStream("deflate"));
  return new Uint8Array(await new Response(stream).arrayBuffer());
}

/** A PNG with a pHYs chunk (pixels per metre) right after IHDR; unchanged if it has one. */
export function pngWithDpi(png: Uint8Array, dpi: number): Uint8Array {
  const isPng = png.length > 33 && png[1] === 0x50 && png[2] === 0x4e && png[3] === 0x47;
  if (!isPng || String.fromCharCode(...png.subarray(12, 16)) !== "IHDR") return png;
  for (let o = 8; o + 8 <= png.length; ) {
    const len = new DataView(png.buffer, png.byteOffset + o).getUint32(0);
    const type = String.fromCharCode(...png.subarray(o + 4, o + 8));
    if (type === "pHYs") return png;
    if (type === "IDAT" || type === "IEND") break;
    o += 12 + len;
  }
  const ppm = Math.round(dpi / 0.0254);
  const chunk = new Uint8Array(21);
  const dv = new DataView(chunk.buffer);
  dv.setUint32(0, 9);
  chunk.set(ascii("pHYs"), 4);
  dv.setUint32(8, ppm);
  dv.setUint32(12, ppm);
  chunk[16] = 1; // unit: metre
  dv.setUint32(17, crc32(chunk.subarray(4, 17)));
  return concat([png.subarray(0, 33), chunk, png.subarray(33)]);
}

/** A JPEG whose JFIF header gives `dpi` (unchanged without a JFIF header). */
export function jpegWithDpi(jpg: Uint8Array, dpi: number): Uint8Array {
  const jfif = jpg.length > 18 && jpg[0] === 0xff && jpg[1] === 0xd8 && jpg[2] === 0xff && jpg[3] === 0xe0 && String.fromCharCode(...jpg.subarray(6, 11)) === "JFIF\0";
  if (!jfif) return jpg;
  const out = jpg.slice();
  const d = Math.max(1, Math.min(65535, Math.round(dpi)));
  out[13] = 1; // unit: dots per inch
  out[14] = d >> 8;
  out[15] = d & 255;
  out[16] = d >> 8;
  out[17] = d & 255;
  return out;
}

/** Pixels as 8-bit RGB, or RGBA with `alpha`. */
function samples(img: ImageData, alpha: boolean): Uint8Array {
  if (alpha) return new Uint8Array(img.data.buffer.slice(img.data.byteOffset, img.data.byteOffset + img.data.byteLength));
  const n = img.width * img.height;
  const out = new Uint8Array(n * 3);
  for (let i = 0; i < n; i++) {
    out[3 * i] = img.data[4 * i];
    out[3 * i + 1] = img.data[4 * i + 1];
    out[3 * i + 2] = img.data[4 * i + 2];
  }
  return out;
}

/** Baseline TIFF: one Deflate strip with the horizontal predictor, RGB or RGBA, DPI tags. */
export async function encodeTiff(img: ImageData, dpi: number, alpha = false): Promise<Uint8Array> {
  const { width: w, height: h } = img;
  const spp = alpha ? 4 : 3;
  const raw = samples(img, alpha);
  const row = w * spp;
  for (let y = 0; y < h; y++) for (let i = row - 1; i >= spp; i--) raw[y * row + i] = (raw[y * row + i] - raw[y * row + i - spp]) & 255;
  const strip = await deflate(raw);
  const res = Math.round(dpi);
  // Layout: header | BitsPerSample values | X and Y resolution | strip | IFD.
  const bitsAt = 8, xresAt = bitsAt + 2 * spp, yresAt = xresAt + 8, stripAt = yresAt + 8;
  const ifdAt = stripAt + strip.length + ((stripAt + strip.length) % 2);
  const tags: [number, number, number, number][] = [
    // [tag, type (3 SHORT, 4 LONG, 5 RATIONAL), count, value or offset]
    [256, 4, 1, w],
    [257, 4, 1, h],
    [258, 3, spp, bitsAt],
    [259, 3, 1, 8],
    [262, 3, 1, 2],
    [273, 4, 1, stripAt],
    [277, 3, 1, spp],
    [278, 4, 1, h],
    [279, 4, 1, strip.length],
    [282, 5, 1, xresAt],
    [283, 5, 1, yresAt],
    [284, 3, 1, 1],
    [296, 3, 1, 2],
    [317, 3, 1, 2],
  ];
  if (alpha) tags.push([338, 3, 1, 2]);
  const out = new Uint8Array(ifdAt + 2 + 12 * tags.length + 4);
  const dv = new DataView(out.buffer);
  out.set(ascii("II"), 0);
  dv.setUint16(2, 42, true);
  dv.setUint32(4, ifdAt, true);
  for (let i = 0; i < spp; i++) dv.setUint16(bitsAt + 2 * i, 8, true);
  dv.setUint32(xresAt, res, true);
  dv.setUint32(xresAt + 4, 1, true);
  dv.setUint32(yresAt, res, true);
  dv.setUint32(yresAt + 4, 1, true);
  out.set(strip, stripAt);
  dv.setUint16(ifdAt, tags.length, true);
  tags.forEach(([tag, type, count, value], i) => {
    const o = ifdAt + 2 + 12 * i;
    dv.setUint16(o, tag, true);
    dv.setUint16(o + 2, type, true);
    dv.setUint32(o + 4, count, true);
    // A single SHORT sits in the low half of the value field; everything else is a LONG or an offset.
    if (type === 3 && count === 1) dv.setUint16(o + 8, value, true);
    else dv.setUint32(o + 8, value, true);
  });
  dv.setUint32(ifdAt + 2 + 12 * tags.length, 0, true);
  return out;
}

/** One-page PDF: the image losslessly (FlateDecode) on a page of its size at `dpi`. */
export async function encodePdf(img: ImageData, dpi: number, alpha = false): Promise<Uint8Array> {
  const { width: w, height: h } = img;
  const rgb = await deflate(samples(img, false));
  let mask: Uint8Array | undefined;
  if (alpha) {
    const a = new Uint8Array(w * h);
    for (let i = 0; i < a.length; i++) a[i] = img.data[4 * i + 3];
    mask = await deflate(a);
  }
  const pt = (px: number) => ((px * 72) / dpi).toFixed(2);
  const content = ascii(`q ${pt(w)} 0 0 ${pt(h)} 0 0 cm /Im0 Do Q\n`);
  const image = (cs: string, len: number, extra = "") =>
    `<< /Type /XObject /Subtype /Image /Width ${w} /Height ${h} /ColorSpace ${cs} /BitsPerComponent 8 /Filter /FlateDecode /Length ${len}${extra} >>`;
  const objects: (Uint8Array | string)[][] = [
    ["<< /Type /Catalog /Pages 2 0 R >>"],
    ["<< /Type /Pages /Kids [3 0 R] /Count 1 >>"],
    [`<< /Type /Page /Parent 2 0 R /MediaBox [0 0 ${pt(w)} ${pt(h)}] /Resources << /XObject << /Im0 4 0 R >> >> /Contents 5 0 R >>`],
    [image("/DeviceRGB", rgb.length, mask ? " /SMask 6 0 R" : ""), "\nstream\n", rgb, "\nendstream"],
    [`<< /Length ${content.length} >>`, "\nstream\n", content, "endstream"],
  ];
  if (mask) objects.push([image("/DeviceGray", mask.length), "\nstream\n", mask, "\nendstream"]);
  // The comment of four bytes above 127 tells tools the file holds binary data.
  const parts: Uint8Array[] = [concat([ascii("%PDF-1.4\n%"), Uint8Array.of(0xe2, 0xe3, 0xcf, 0xd3), ascii("\n")])];
  const offsets: number[] = [];
  let at = parts[0].length;
  objects.forEach((obj, i) => {
    offsets.push(at);
    const bytes = concat([ascii(`${i + 1} 0 obj\n`), ...obj.map((x) => (typeof x === "string" ? ascii(x) : x)), ascii("\nendobj\n")]);
    parts.push(bytes);
    at += bytes.length;
  });
  const xref = [`xref\n0 ${objects.length + 1}\n0000000000 65535 f \n`, ...offsets.map((o) => `${String(o).padStart(10, "0")} 00000 n \n`)].join("");
  parts.push(ascii(`${xref}trailer\n<< /Size ${objects.length + 1} /Root 1 0 R >>\nstartxref\n${at}\n%%EOF\n`));
  return concat(parts);
}
