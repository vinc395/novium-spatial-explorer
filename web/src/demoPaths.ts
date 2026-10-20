// The online demo (GitHub Pages) has no server: every API reply the viewer needs was
// saved as a file by scripts/export_demo.ts. This maps an API URL to that file, for both
// the exporter and the viewer. Query strings are dropped: transcript tiles hold every gene
// (the viewer keeps the ones asked for), density tiles were saved at the default QV 20,
// and cell details hold every field. Replies ending in `.bin` are stored gzipped as
// `.bin.gz` and unpacked by the viewer.

/** File-name-safe form of an ID (letters, digits, `_ . -` kept; anything else `~hex`). */
export function safeName(s: string): string {
  return s.replace(/[^A-Za-z0-9_.-]/g, (c) => `~${c.charCodeAt(0).toString(16)}`);
}

/** Path of the saved reply for `url` (relative to the demo's `api/` folder), or null. */
export function demoPath(url: string): string | null {
  const [path, query = ""] = url.split("?", 2);
  if (!path.startsWith("/api/v1/")) return null;
  const p = path.slice("/api/v1/".length).split("/").map(decodeURIComponent);
  if (p.length === 1 && (p[0] === "sections" || p[0] === "session")) return `${p[0]}.json`;
  if (p.join("/") === "ucell/signatures") return "ucell/signatures.json";
  if (p[0] !== "sections" || p.length < 3) return null;
  const sec = `sections/${safeName(p[1])}`;
  const r = p.slice(2);
  const n = (s: string) => /^-?\d+$/.test(s);
  if (r.length === 1 && r[0] === "manifest") return `${sec}/manifest.json`;
  if (r[0] === "cells") {
    if (r.length === 2 && r[1] === "centroids") return `${sec}/cells/centroids.bin`;
    if (r.length === 3 && r[1] === "fields") return `${sec}/cells/fields/${safeName(r[2])}.bin`;
    if (r.length === 3 && r[1] === "key" && n(r[2])) return `${sec}/cells/key/${r[2]}.json`;
    if (r.length === 2 && r[1] === "lookup") {
      const id = new URLSearchParams(query).get("id");
      return id ? `${sec}/cells/lookup/${safeName(id)}.json` : null;
    }
    if (r.length === 5 && r[1] === "chunks" && (r[2] === "cell" || r[2] === "nucleus") && n(r[3]) && n(r[4]))
      return `${sec}/cells/chunks/${r[2]}/${r[3]}_${r[4]}.bin`;
  }
  if (r.length === 2 && r[0] === "expression" && n(r[1])) return `${sec}/expression/${r[1]}.bin`;
  if (r[0] === "transcripts") {
    if (r.length === 4 && r[1] === "genes" && n(r[2]) && r[3] === "tiles") return `${sec}/transcripts/genes/${r[2]}.bin`;
    if (r.length === 4 && r[1] === "tiles" && n(r[2]) && n(r[3])) return `${sec}/transcripts/tiles/${r[2]}_${r[3]}.bin`;
    if (r.length === 6 && r[1] === "density" && r.slice(2).every(n)) return `${sec}/transcripts/density/${r[2]}/${r[3]}/${r[4]}_${r[5]}.bin`;
  }
  if (r.length === 5 && r[0] === "images" && r.slice(2).every(n)) return `${sec}/images/${safeName(r[1])}/${r[2]}/${r[3]}_${r[4]}.jpg`;
  if (r.length === 3 && r[0] === "grids") return `${sec}/grids/${safeName(r[1])}/${safeName(r[2])}.bin`;
  if (r.length === 2 && r[0] === "ucell" && r[1] === "results") return `${sec}/ucell/results.json`;
  return null;
}
