// Same-origin API client. Every data URL carries ?v=<index version> so the
// browser may cache immutable responses safely across re-ingests.

import type { CellDetails, Coverage, Manifest, SectionSummary, Signature, UcellJob, UcellParams, UcellResult, UcellSummary } from "./types";
import { MIN_SCHEMA_VERSION, SCHEMA_VERSION } from "./types";

export class ApiError extends Error {
  constructor(public status: number, message: string) {
    super(message);
  }
}

async function check(r: Response): Promise<Response> {
  if (!r.ok) {
    let msg = `${r.status} ${r.statusText}`;
    try {
      const j = await r.json();
      if (j?.error) msg = j.error;
    } catch {
      /* not json */
    }
    throw new ApiError(r.status, msg);
  }
  return r;
}

export async function getJson<T>(url: string, signal?: AbortSignal): Promise<T> {
  return (await check(await fetch(url, { signal }))).json();
}

export async function getBuffer(url: string, signal?: AbortSignal): Promise<ArrayBuffer> {
  return (await check(await fetch(url, { signal }))).arrayBuffer();
}

async function send<T>(method: string, url: string, body?: unknown): Promise<T> {
  const r = await check(
    await fetch(url, {
      method,
      headers: body === undefined ? undefined : { "content-type": "application/json" },
      body: body === undefined ? undefined : JSON.stringify(body),
    }),
  );
  return (r.status === 204 ? undefined : await r.json()) as T;
}

export interface SignatureInput { name: string; description?: string; genes: string[] }
export interface JobRequest {
  signature_ids: string[];
  params: UcellParams;
  level: "cell" | "bin";
  bin_um?: number;
  min_transcripts?: number;
}

/** Signatures and jobs are global (not per section). */
export const ucellApi = {
  signatures: async () => (await getJson<{ signatures: Signature[] }>("/api/v1/ucell/signatures")).signatures,
  create: (s: SignatureInput) => send<Signature>("POST", "/api/v1/ucell/signatures", s),
  update: (id: string, s: SignatureInput) => send<Signature>("PUT", `/api/v1/ucell/signatures/${encodeURIComponent(id)}`, s),
  remove: (id: string) => send<void>("DELETE", `/api/v1/ucell/signatures/${encodeURIComponent(id)}`),
  job: (id: string) => getJson<UcellJob>(`/api/v1/ucell/jobs/${encodeURIComponent(id)}`),
  cancel: (id: string) => send<UcellJob>("DELETE", `/api/v1/ucell/jobs/${encodeURIComponent(id)}`),
};

/** Whether this server requires sign-in (token link or password), i.e. whether "Sign out" applies. */
export async function session(): Promise<{ auth: boolean; password?: boolean; user?: string | null }> {
  return getJson("/api/v1/session");
}

export async function listSections(): Promise<SectionSummary[]> {
  return (await getJson<{ sections: SectionSummary[] }>("/api/v1/sections")).sections;
}

export class SectionApi {
  readonly base: string;
  constructor(readonly sectionId: string, readonly version: string) {
    this.base = `/api/v1/sections/${encodeURIComponent(sectionId)}`;
  }
  private v(extra = ""): string {
    return `?v=${encodeURIComponent(this.version)}${extra}`;
  }
  async manifest(): Promise<Manifest> {
    const m = await getJson<Manifest>(`${this.base}/manifest`);
    if (!(m.schema_version >= MIN_SCHEMA_VERSION && m.schema_version <= SCHEMA_VERSION))
      throw new Error(`unsupported index schema_version ${m.schema_version} (reload the page if the explorer was updated)`);
    return m;
  }
  centroidsUrl() { return `${this.base}/cells/centroids${this.v()}`; }
  fieldUrl(id: string) { return `${this.base}/cells/fields/${encodeURIComponent(id)}${this.v()}`; }
  expressionUrl(gene: number) { return `${this.base}/expression/${gene}${this.v()}`; }
  chunkUrl(kind: "cell" | "nucleus", tx: number, ty: number) { return `${this.base}/cells/chunks/${kind}/${tx}/${ty}${this.v()}`; }
  pointsUrl(tx: number, ty: number, genes: number[]) {
    return `${this.base}/transcripts/tiles/${tx}/${ty}${this.v(`&genes=${genes.join(",")}`)}`;
  }
  geneTilesUrl(gene: number) { return `${this.base}/transcripts/genes/${gene}/tiles${this.v()}`; }
  densityUrl(gene: number, level: number, x: number, y: number, qvMin: number) {
    return `${this.base}/transcripts/density/${gene}/${level}/${x}/${y}${this.v(`&qv_min=${qvMin}`)}`;
  }
  imageTileUrl(image: string, level: number, x: number, y: number) {
    return `${this.base}/images/${encodeURIComponent(image)}/${level}/${x}/${y}${this.v()}`;
  }
  gridUrl(grid: string, field: string) {
    return `${this.base}/grids/${encodeURIComponent(grid)}/${encodeURIComponent(field)}${this.v()}`;
  }
  /** Cell details; `groups` = only those field groups' values, `excludeGroups` = all but those. */
  cell(key: number, signal?: AbortSignal, q: { groups?: string[]; excludeGroups?: string[] } = {}) {
    const p = new URLSearchParams();
    if (q.groups) p.set("groups", q.groups.join(","));
    if (q.excludeGroups) p.set("exclude_groups", q.excludeGroups.join(","));
    const qs = p.toString();
    return getJson<CellDetails>(`${this.base}/cells/key/${key}${qs ? `?${qs}` : ""}`, signal);
  }
  lookup(id: string) {
    return getJson<{ cell_key: number; cell_id: string; centroid_um: [number, number] }>(
      `${this.base}/cells/lookup?id=${encodeURIComponent(id)}`,
    );
  }
  ucellCoverage(genes: string[], params: UcellParams) {
    return send<Coverage>("POST", `${this.base}/ucell/coverage`, { genes, params });
  }
  ucellSubmit(req: JobRequest) { return send<UcellJob>("POST", `${this.base}/ucell/jobs`, req); }
  async ucellResults() { return (await getJson<{ results: UcellResult[] }>(`${this.base}/ucell/results`)).results; }
  ucellDelete(key: string) { return send<void>("DELETE", `${this.base}/ucell/results/${encodeURIComponent(key)}`); }
  ucellValuesUrl(key: string) { return `${this.base}/ucell/results/${encodeURIComponent(key)}/values${this.v()}`; }
  ucellSummary(key: string, by: string) {
    return getJson<UcellSummary>(`${this.base}/ucell/results/${encodeURIComponent(key)}/summary?by=${encodeURIComponent(by)}`);
  }
  ucellExportUrl(keys: string[]) { return `${this.base}/ucell/export.parquet?keys=${keys.map(encodeURIComponent).join(",")}`; }
  async exportCsv(cellKeys: number[], fields: string[]): Promise<Blob> {
    const r = await check(
      await fetch(`${this.base}/selection/export`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ cell_keys: cellKeys, fields }),
      }),
    );
    return r.blob();
  }
}
