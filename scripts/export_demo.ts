// Save every API reply the viewer needs for the online demo, which has no server (GitHub
// Pages): the section list, then per section the manifest, cells and fields, outlines,
// transcripts (all genes per tile), density tiles (at the default QV 20), expression,
// images, grids and every cell's details. File paths come from web/src/demoPaths.ts.
// Only for small synthetic sections (see scripts/build_demo_site.sh):
//   node --experimental-strip-types scripts/export_demo.ts http://127.0.0.1:18765 site/api
import { mkdir, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { gzipSync } from "node:zlib";
import { demoPath } from "../web/src/demoPaths.ts";

const [base, out] = process.argv.slice(2);
if (!base || !out) {
  console.error("usage: export_demo.ts <server URL> <output dir>");
  process.exit(2);
}
let files = 0;
let bytes = 0;

/** Fetch `url` from the server (or take `body`) and save it at its demo path. */
async function save(url: string, body?: Uint8Array): Promise<Uint8Array> {
  const rel = demoPath(url);
  if (!rel) throw new Error(`no demo path for ${url}`);
  if (!body) {
    const r = await fetch(base + url);
    if (!r.ok) throw new Error(`${url}: HTTP ${r.status}`);
    body = new Uint8Array(await r.arrayBuffer());
  }
  // Binary replies are stored gzipped (`.bin.gz`; the server compresses them on the fly,
  // a static host would not): density tiles are mostly zeros.
  const gz = rel.endsWith(".bin");
  const data = gz ? gzipSync(body, { level: 9 }) : body;
  const p = join(out, gz ? `${rel}.gz` : rel);
  await mkdir(dirname(p), { recursive: true });
  await writeFile(p, data);
  files++;
  bytes += data.length;
  return body;
}
const json = async (url: string) => JSON.parse(new TextDecoder().decode(await save(url)));
const encode = (o: unknown) => new TextEncoder().encode(JSON.stringify(o));
const range = (a: number, b: number) => Array.from({ length: Math.max(0, b - a + 1) }, (_, i) => a + i);
const grid = (xs: number[], ys: number[]) => xs.flatMap((x) => ys.map((y) => [x, y]));

/** Run `f` over `items`, 16 at a time. */
async function each<T>(items: T[], f: (t: T) => Promise<unknown>) {
  let i = 0;
  await Promise.all(Array.from({ length: 16 }, async () => {
    while (i < items.length) await f(items[i++]);
  }));
}

await save("/api/v1/session", encode({ auth: false }));
await save("/api/v1/ucell/signatures", encode({ signatures: [] }));
const { sections } = await json("/api/v1/sections");
for (const s of sections) {
  const S = `/api/v1/sections/${encodeURIComponent(s.section_id)}`;
  const m = await json(`${S}/manifest`);
  const [b0, b1, b2, b3] = m.bounds_um as number[];
  const ts = m.grid.tile_size_um as number;
  await save(`${S}/ucell/results`, encode({ results: [] }));
  await save(`${S}/cells/centroids`);
  await each(m.cells.fields, (f: { id: string }) => save(`${S}/cells/fields/${encodeURIComponent(f.id)}`));
  await each(range(1, m.cells.count), async (k) => {
    const d = await json(`${S}/cells/key/${k}`);
    await save(`${S}/cells/lookup?id=${encodeURIComponent(d.cell_id)}`, encode({ cell_key: k, cell_id: d.cell_id, centroid_um: d.centroid_um }));
  });
  // Every outline chunk in reach of the view, empty ones included (as the server sends them).
  for (const kind of ["cell", "nucleus"]) {
    const layer = kind === "cell" ? m.cells.boundaries : m.cells.nuclei;
    if (!layer) continue;
    const mg = layer.query_margin_um as number;
    const xs = range(Math.floor((b0 - mg) / ts), Math.floor((b2 + mg) / ts));
    const ys = range(Math.floor((b1 - mg) / ts), Math.floor((b3 + mg) / ts));
    await each(grid(xs, ys), ([x, y]) => save(`${S}/cells/chunks/${kind}/${x}/${y}`));
  }
  const t = m.transcripts;
  if (t) {
    const ids = (t.genes as { id: number }[]).map((g) => g.id);
    if (ids.length > 64) throw new Error("the demo exporter handles panels of up to 64 features (one request per tile)");
    await each(ids, (g) => save(`${S}/expression/${g}`));
    await each(ids, (g) => save(`${S}/transcripts/genes/${g}/tiles`));
    await each(grid(range(Math.floor(b0 / ts), Math.floor(b2 / ts)), range(Math.floor(b1 / ts), Math.floor(b3 / ts))), ([x, y]) =>
      save(`${S}/transcripts/tiles/${x}/${y}?genes=${ids.join(",")}`),
    );
    const jobs: number[][] = [];
    for (let l = 0; l <= t.density_max_level; l++) {
      const size = t.density_tile_bins * 2 ** l;
      for (const [x, y] of grid(range(Math.floor(b0 / size), Math.floor(b2 / size)), range(Math.floor(b1 / size), Math.floor(b3 / size))))
        for (const g of ids) jobs.push([g, l, x, y]);
    }
    await each(jobs, ([g, l, x, y]) => save(`${S}/transcripts/density/${g}/${l}/${x}/${y}?qv_min=20`));
  }
  for (const im of m.images) {
    const jobs: number[][] = [];
    for (let l = 0; l < im.levels; l++) {
      const span = im.tile_size * 2 ** l;
      for (const [x, y] of grid(range(0, Math.ceil(im.width / span) - 1), range(0, Math.ceil(im.height / span) - 1))) jobs.push([l, x, y]);
    }
    await each(jobs, ([l, x, y]) => save(`${S}/images/${encodeURIComponent(im.id)}/${l}/${x}/${y}`));
  }
  for (const g of m.grids) for (const f of g.fields) await save(`${S}/grids/${encodeURIComponent(g.id)}/${encodeURIComponent(f.id)}`);
}
console.log(`demo: ${sections.length} sections, ${files} files, ${(bytes / 1e6).toFixed(1)} MB in ${out}`);
