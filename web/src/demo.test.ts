import { describe, expect, it } from "vitest";
import { decodePoints, POINT_RECORD_BYTES, selectPointGenes } from "./codecs";
import { demoPath, safeName } from "./demoPaths";

describe("online demo file paths", () => {
  const S = "/api/v1/sections/DEMO1";
  it("map every viewer request to a saved file, ignoring query strings", () => {
    const cases: [string, string | null][] = [
      ["/api/v1/sections", "sections.json"],
      ["/api/v1/session", "session.json"],
      ["/api/v1/ucell/signatures", "ucell/signatures.json"],
      [`${S}/manifest`, "sections/DEMO1/manifest.json"],
      [`${S}/cells/centroids?v=v1`, "sections/DEMO1/cells/centroids.bin"],
      [`${S}/cells/fields/lr%3AP1%3Asender?v=v1`, "sections/DEMO1/cells/fields/lr~3aP1~3asender.bin"],
      [`${S}/cells/key/12?exclude_groups=lr_score,lr_hotspot`, "sections/DEMO1/cells/key/12.json"],
      [`${S}/cells/lookup?id=abcd-1`, "sections/DEMO1/cells/lookup/abcd-1.json"],
      [`${S}/cells/chunks/nucleus/-1/2?v=v1`, "sections/DEMO1/cells/chunks/nucleus/-1_2.bin"],
      [`${S}/expression/7?v=v1`, "sections/DEMO1/expression/7.bin"],
      [`${S}/transcripts/genes/7/tiles?v=v1`, "sections/DEMO1/transcripts/genes/7.bin"],
      [`${S}/transcripts/tiles/1/2?v=v1&genes=3,7`, "sections/DEMO1/transcripts/tiles/1_2.bin"],
      [`${S}/transcripts/density/7/1/0/1?v=v1&qv_min=30`, "sections/DEMO1/transcripts/density/7/1/0_1.bin"],
      [`${S}/images/morph-dapi/2/1/0?v=v1`, "sections/DEMO1/images/morph-dapi/2/1_0.jpg"],
      [`${S}/grids/niches/niche_label?v=v1`, "sections/DEMO1/grids/niches/niche_label.bin"],
      [`${S}/ucell/results`, "sections/DEMO1/ucell/results.json"],
      [`${S}/ucell/jobs`, null],
      [`${S}/selection/export`, null],
      ["/healthz", null],
    ];
    for (const [url, path] of cases) expect(demoPath(url), url).toBe(path);
  });

  it("keep file names safe and distinct", () => {
    expect(safeName("a:b/c d~")).toBe("a~3ab~2fc~20d~7e");
    expect(safeName("plain_name-1.x")).toBe("plain_name-1.x");
  });
});

describe("demo transcript tiles", () => {
  // XPT1 with genes 3 (2 records) and 7 (1 record); each record's first byte marks it.
  function tile(): ArrayBuffer {
    const segs = [[3, 2], [7, 1]];
    const buf = new ArrayBuffer(16 + 8 * segs.length + POINT_RECORD_BYTES * 3);
    const dv = new DataView(buf);
    new Uint8Array(buf).set([0x58, 0x50, 0x54, 0x31]); // "XPT1"
    dv.setUint16(4, 1, true);
    dv.setUint32(8, 3, true);
    dv.setUint32(12, segs.length, true);
    segs.forEach(([g, n], i) => {
      dv.setUint32(16 + 8 * i, g, true);
      dv.setUint32(20 + 8 * i, n, true);
    });
    const at = 16 + 8 * segs.length;
    [11, 12, 13].forEach((mark, i) => dv.setUint8(at + POINT_RECORD_BYTES * i, mark));
    return buf;
  }

  it("keep only the requested genes' records", () => {
    const t = decodePoints(selectPointGenes(tile(), [7]));
    expect(t.segments).toEqual([{ geneId: 7, count: 1 }]);
    expect(t.records.getUint8(0)).toBe(13);
    const both = decodePoints(selectPointGenes(tile(), [3, 7]));
    expect(both.count).toBe(3);
    expect(decodePoints(selectPointGenes(tile(), [99])).count).toBe(0);
  });
});
