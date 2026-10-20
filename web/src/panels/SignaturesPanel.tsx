// Gene signatures and UCell scores: define/edit signatures (with live panel
// coverage), score them in every open section (cell or bin level), follow jobs, and
// use the stored results for colouring, overlays, heatmaps, niche comparisons and
// Parquet export.

import { useEffect, useMemo, useState } from "react";
import { DEMO, ucellApi } from "../api";
import { deleteUcellResult, loadSignatures, scoreSelected, ucellLabel } from "../data";
import { activePanel, getState, setState, setUcell, useStore } from "../store";
import { COLORMAPS } from "../colors";
import type { Coverage, UcellResult, UcellSummary } from "../types";

const parseGenes = (text: string) => text.split(/[\s,;]+/).map((g) => g.trim()).filter(Boolean);
const pct = (x: number | null) => (x === null ? "–" : `${(x * 100).toFixed(x < 0.1 ? 1 : 0)}%`);

interface Draft {
  id?: string;
  name: string;
  genes: string;
  description: string;
}

export function SignaturesPanel() {
  return DEMO ? <DemoNote /> : <LiveSignaturesPanel />;
}

/** The online demo has no server to score with. */
function DemoNote() {
  return (
    <section className="ucell" aria-label="Gene signatures">
      <h3>Gene signatures (UCell)</h3>
      <p className="muted small">
        Scoring gene signatures runs on the server, so it is not part of this online demo. Install NOVIUM Spatial Explorer to
        score signatures in your own data.
      </p>
    </section>
  );
}

function LiveSignaturesPanel() {
  const u = useStore((s) => s.ucell);
  const p = useStore((s) => activePanel(s));
  const sections = useStore((s) => [...new Set(s.panels.filter((x) => x.manifest).map((x) => x.sectionId))].length);
  const [draft, setDraft] = useState<Draft | null>(null);
  useEffect(() => {
    loadSignatures();
  }, []);
  const results = (p.sectionId && u.results[p.sectionId]) || [];
  const live = u.jobs.filter((j) => j.status === "queued" || j.status === "running");
  const recent = u.jobs.filter((j) => j.status !== "queued" && j.status !== "running").slice(0, 3);
  const toggle = (id: string) =>
    setUcell((x) => ({ selected: x.selected.includes(id) ? x.selected.filter((y) => y !== id) : [...x.selected, id] }));
  const params = u.params;
  const setParams = (patch: Partial<typeof params>) => setUcell((x) => ({ params: { ...x.params, ...patch } }));
  if (!p.manifest) return null;
  const panelGenes = p.manifest.transcripts?.genes.filter((g) => g.category === "gene").length ?? 0;
  return (
    <section className="ucell" aria-label="Gene signatures">
      <h3>Gene signatures (UCell)</h3>
      {u.error && <p className="error small">{u.error}</p>}
      {u.signatures.length === 0 && !draft && <p className="muted small">No signatures yet.</p>}
      <ul className="sigs">
        {u.signatures.map((sig) => (
          <li key={sig.id}>
            <label className="check" title={sig.genes.join(" ")}>
              <input type="checkbox" checked={u.selected.includes(sig.id)} onChange={() => toggle(sig.id)} aria-label={`select ${sig.name}`} />
              <span className="name">{sig.name}</span>
              <span className="muted small">{sig.genes.length} genes</span>
            </label>
            <button className="link" onClick={() => setDraft({ id: sig.id, name: sig.name, genes: sig.genes.join(" "), description: sig.description })}>
              edit
            </button>
          </li>
        ))}
      </ul>
      {draft ? (
        <SignatureEditor draft={draft} onClose={() => setDraft(null)} />
      ) : (
        <button onClick={() => setDraft({ name: "", genes: "", description: "" })}>New signature</button>
      )}

      <details className="ucell-settings">
        <summary>
          Scoring: {u.level === "cell" ? "cells" : `${u.binUm} µm bins`} · maxRank {Math.min(params.max_rank, panelGenes || params.max_rank)} · {params.missing_genes}
        </summary>
        <label className="inline">
          level
          <select value={u.level} onChange={(e) => setUcell({ level: e.target.value as "cell" })} aria-label="UCell level">
            <option value="cell">cells (each cell's own counts)</option>
            <option value="bin">bins (pooled transcripts)</option>
          </select>
        </label>
        {u.level === "bin" && (
          <div className="row small">
            <label>bin µm <input type="number" min={2} max={2000} value={u.binUm} onChange={(e) => setUcell({ binUm: Number(e.target.value) })} /></label>
            <label>min transcripts <input type="number" min={1} value={u.minTranscripts} onChange={(e) => setUcell({ minTranscripts: Number(e.target.value) })} /></label>
          </div>
        )}
        <div className="row small">
          <label title="Ranks above maxRank count as maxRank (UCell default 1500); capped at the panel size">
            maxRank <input type="number" min={1} value={params.max_rank} onChange={(e) => setParams({ max_rank: Math.max(1, Math.round(Number(e.target.value))) })} aria-label="maxRank" />
          </label>
          <label title="Weight of the negative (gene-) part">
            w_neg <input type="number" min={0} step={0.1} value={params.w_neg} onChange={(e) => setParams({ w_neg: Math.max(0, Number(e.target.value)) })} />
          </label>
        </div>
        <label className="inline small">
          genes not in the panel
          <select value={params.missing_genes} onChange={(e) => setParams({ missing_genes: e.target.value as "impute" })}>
            <option value="impute">impute (count at maxRank; UCell default)</option>
            <option value="skip">skip (drop from the signature)</option>
          </select>
        </label>
        <p className="muted small">
          Ranks use all {panelGenes.toLocaleString()} panel genes of each {u.level === "cell" ? "cell" : "bin"} (QV≥20), never only the selected genes. Same
          algorithm as R UCell 2.14 (identical scores).
        </p>
      </details>
      <button className="primary" disabled={!u.selected.length} onClick={() => scoreSelected()}>
        Score {u.selected.length || ""} signature{u.selected.length === 1 ? "" : "s"}
        {sections > 1 ? ` in ${sections} sections` : ""}
      </button>

      {[...live, ...recent].map((j) => (
        <div key={j.id} className={`job ${j.status}`} aria-label={`job ${j.status}`}>
          <div className="row small">
            <span>
              {j.section_id} · {j.signatures.join(", ")} · {j.status}
              {j.status === "running" && j.phase ? ` (${j.phase})` : ""}
            </span>
            {(j.status === "queued" || j.status === "running") && (
              <button className="link" onClick={() => ucellApi.cancel(j.id).catch(() => {})} aria-label="cancel job">
                cancel
              </button>
            )}
          </div>
          {j.status === "running" && <progress max={j.total || 1} value={j.done} />}
          {j.error && <p className="error small">{j.error}</p>}
        </div>
      ))}

      {results.length > 0 && <ResultsList results={results} />}
    </section>
  );
}

function SignatureEditor({ draft: d0, onClose }: { draft: Draft; onClose: () => void }) {
  const p = useStore((s) => activePanel(s));
  const params = useStore((s) => s.ucell.params);
  const [d, setD] = useState(d0);
  const [cov, setCov] = useState<Coverage>();
  const [err, setErr] = useState<string>();
  const genes = useMemo(() => parseGenes(d.genes), [d.genes]);
  useEffect(() => {
    if (!p.api || !genes.length) {
      setCov(undefined);
      return;
    }
    const t = setTimeout(() => {
      p.api!.ucellCoverage(genes, params).then(setCov, (e) => setErr(String(e)));
    }, 250);
    return () => clearTimeout(t);
  }, [genes, params, p.api]);
  const save = async () => {
    setErr(undefined);
    try {
      const body = { name: d.name, description: d.description, genes };
      const sig = d.id ? await ucellApi.update(d.id, body) : await ucellApi.create(body);
      await loadSignatures();
      if (!d.id) setUcell((u) => ({ selected: [...u.selected, sig.id] }));
      onClose();
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    }
  };
  const remove = async () => {
    if (!d.id || !confirm(`Delete signature “${d.name}”? Stored scores stay until the section is re-indexed.`)) return;
    await ucellApi.remove(d.id).catch((e) => setErr(String(e)));
    await loadSignatures();
    onClose();
  };
  const fix = (from: string, to: string) =>
    setD({ ...d, genes: parseGenes(d.genes).map((g) => (g.replace(/[+-]$/, "") === from ? to + (g.match(/[+-]$/)?.[0] ?? "") : g)).join(" ") });
  return (
    <div className="sig-editor" aria-label="Signature editor">
      <input placeholder="Signature name" value={d.name} onChange={(e) => setD({ ...d, name: e.target.value })} aria-label="Signature name" />
      <textarea
        rows={4}
        placeholder={"Genes, separated by spaces, commas or new lines.\nAppend - for negative genes, e.g. CD3E CD2 CD19-"}
        value={d.genes}
        onChange={(e) => setD({ ...d, genes: e.target.value })}
        aria-label="Signature genes"
      />
      <input placeholder="Description (optional)" value={d.description} onChange={(e) => setD({ ...d, description: e.target.value })} />
      {cov && (
        <div className="coverage small" aria-label="Gene coverage">
          <div>
            <strong>{p.sectionId}</strong>: {cov.positive_found}/{cov.positive_total} positive
            {cov.negative_total > 0 && `, ${cov.negative_found}/${cov.negative_total} negative`} genes in the panel ({cov.panel_genes.toLocaleString()} genes)
            {cov.imputed > 0 && ` · ${cov.imputed} imputed at maxRank`}
          </div>
          {cov.error && <div className="error">{cov.error}</div>}
          <ul className="cov-genes">
            {cov.genes.map((g, i) => (
              <li key={i} className={g.status} title={g.status === "found" ? `${g.cells_detected?.toLocaleString()} cells, ${g.transcripts_qv20?.toLocaleString()} transcripts (QV≥20)` : g.status}>
                {g.gene}
                {g.sign === "-" ? "−" : ""}
                <span className="muted"> {g.status === "found" ? pct(g.fraction_detected) : g.status === "not_a_gene" ? "control" : "missing"}</span>
                {g.suggestion && (
                  <button className="link" onClick={() => fix(g.gene, g.suggestion!)}>
                    → {g.suggestion}
                  </button>
                )}
              </li>
            ))}
          </ul>
          <div className="muted">% = cells with ≥1 transcript of the gene in this section</div>
        </div>
      )}
      {err && <p className="error small">{err}</p>}
      <div className="row">
        <button className="primary" disabled={!d.name.trim() || !genes.length} onClick={save}>
          {d.id ? "Save" : "Create"}
        </button>
        <button onClick={onClose}>Cancel</button>
        {d.id && (
          <button className="link danger" onClick={remove}>
            delete
          </button>
        )}
      </div>
    </div>
  );
}

function ResultsList({ results }: { results: UcellResult[] }) {
  const p = useStore((s) => activePanel(s));
  const heat = useStore((s) => s.ucell.heat);
  const colorSource = useStore((s) => s.colorBy.source);
  const overlaySource = useStore((s) => s.overlay.source);
  const [picked, setPicked] = useState<string[]>([]);
  const [niche, setNiche] = useState<string>();
  const pickedResults = results.filter((r) => picked.includes(r.key));
  const exportable = pickedResults.length > 0 && pickedResults.every((r) => r.level === pickedResults[0].level);
  const [deleteErr, setDeleteErr] = useState<string>();
  const remove = async (r: UcellResult) => {
    const where = r.level === "cell" ? "cell-level" : `${r.bin_um} µm bin`;
    if (!confirm(`Delete the stored ${where} scores of “${r.signature.name}” for ${p.sectionId}? Scoring again recreates them.`)) return;
    setDeleteErr(undefined);
    setPicked((x) => x.filter((k) => k !== r.key));
    if (niche === r.key) setNiche(undefined);
    await deleteUcellResult(p.sectionId!, r.key).catch((e) => setDeleteErr(e instanceof Error ? e.message : String(e)));
  };
  const showHeat = (r: UcellResult) =>
    setUcell((u) => ({ heat: { ...u.heat, key: r.key, mode: r.level === "bin" ? "bin" : "cell-mean", visible: true } }));
  return (
    <div className="ucell-results">
      <h4>
        Scores · {p.sectionId} <span className="muted small">({results.length})</span>
      </h4>
      <ul>
        {results.map((r) => {
          const src = `ucell:${r.key}`;
          return (
            <li key={r.key}>
              <label className="check small" title={`${r.signature.genes.join(" ")}\nkey ${r.key} · ${r.algorithm} · ${r.duration_ms} ms`}>
                <input
                  type="checkbox"
                  checked={picked.includes(r.key)}
                  onChange={() => setPicked((x) => (x.includes(r.key) ? x.filter((k) => k !== r.key) : [...x, r.key]))}
                  aria-label={`pick ${r.signature.name}`}
                />
                <span className="name">{r.signature.name}</span>
                <span className="tag">{r.level === "cell" ? "cells" : `${r.bin_um} µm bins`}</span>
                <span className="muted">
                  maxRank {r.coverage.max_rank_effective} · {r.coverage.positive_found}/{r.coverage.positive_total} genes
                </span>
              </label>
              <div className="row small actions">
                {r.level === "cell" && (
                  <>
                    <button className={`link${colorSource === src ? " on" : ""}`} onClick={() => setState({ colorBy: { ...getState().colorBy, source: src, range: null, hidden: new Set() } })}>
                      colour cells
                    </button>
                    <button className={`link${overlaySource === src ? " on" : ""}`} onClick={() => setState({ overlay: { ...getState().overlay, source: src, range: null, hidden: new Set() } })}>
                      overlay
                    </button>
                    <button className={`link${niche === r.key ? " on" : ""}`} onClick={() => setNiche(niche === r.key ? undefined : r.key)}>
                      by niche / type
                    </button>
                  </>
                )}
                <button className={`link${heat.key === r.key && heat.visible ? " on" : ""}`} onClick={() => showHeat(r)}>
                  heatmap
                </button>
                <button className="link danger" onClick={() => remove(r)} aria-label={`delete ${r.signature.name} scores`}>
                  delete
                </button>
              </div>
              {niche === r.key && <NicheSummary result={r} />}
            </li>
          );
        })}
      </ul>
      {deleteErr && <p className="error small">{deleteErr}</p>}
      <div className="row small">
        <a
          className={`button${exportable ? "" : " disabled"}`}
          href={exportable ? p.api!.ucellExportUrl(picked) : undefined}
          download
          aria-disabled={!exportable}
        >
          Export Parquet{picked.length ? ` (${picked.length})` : ""}
        </a>
        {!exportable && picked.length > 0 && <span className="muted">pick results of one level</span>}
      </div>
      {heat.key && (
        <div className="ucell-heat">
          <div className="row small">
            <label className="check">
              <input type="checkbox" checked={heat.visible} onChange={(e) => setUcell((u) => ({ heat: { ...u.heat, visible: e.target.checked } }))} /> heatmap
            </label>
            {heat.mode === "cell-mean" && (
              <label>
                bin µm{" "}
                <input type="number" min={5} max={2000} value={heat.binUm} onChange={(e) => setUcell((u) => ({ heat: { ...u.heat, binUm: Math.max(5, Number(e.target.value)) } }))} aria-label="Heatmap bin size" />
              </label>
            )}
            <select value={heat.colormap} onChange={(e) => setUcell((u) => ({ heat: { ...u.heat, colormap: e.target.value } }))} aria-label="Heatmap colormap">
              {COLORMAPS.map((c) => (
                <option key={c}>{c}</option>
              ))}
            </select>
          </div>
          <label className="slider">
            <span>Opacity</span>
            <input type="range" min={0} max={1} step={0.05} value={heat.opacity} onChange={(e) => setUcell((u) => ({ heat: { ...u.heat, opacity: Number(e.target.value) } }))} />
            <span className="val">{heat.opacity.toFixed(2)}</span>
          </label>
        </div>
      )}
    </div>
  );
}

function NicheSummary({ result }: { result: UcellResult }) {
  const p = useStore((s) => activePanel(s));
  const m = p.manifest!;
  const options = useMemo(() => {
    const o: { value: string; label: string }[] = [];
    for (const g of m.grids)
      for (const f of g.fields) if (f.kind === "categorical") o.push({ value: `grid:${g.id}/${f.id}`, label: `${g.label}: ${f.label} (grid)` });
    for (const f of m.cells?.fields ?? []) if (f.kind === "categorical") o.push({ value: `field:${f.id}`, label: `${f.label} (cell field)` });
    return o;
  }, [m]);
  const [by, setBy] = useState(options[0]?.value ?? "");
  const [sum, setSum] = useState<UcellSummary>();
  const [err, setErr] = useState<string>();
  useEffect(() => {
    if (!by || !p.api) return;
    setSum(undefined);
    setErr(undefined);
    p.api.ucellSummary(result.key, by).then(setSum, (e) => setErr(String(e)));
  }, [by, result.key, p.api]);
  if (!options.length) return <p className="muted small">No categorical cell field or niche grid to compare.</p>;
  const groups = [...(sum?.groups ?? [])].sort((a, b) => b.median - a.median || b.mean - a.mean);
  const hi = Math.max(1e-9, ...groups.map((g) => g.p95));
  const x = (v: number) => `${(Math.min(v, hi) / hi) * 100}%`;
  return (
    <div className="niche-summary" aria-label="UCell by category">
      <select value={by} onChange={(e) => setBy(e.target.value)} aria-label="Compare by">
        {options.map((o) => (
          <option key={o.value} value={o.value}>
            {o.label}
          </option>
        ))}
      </select>
      {err && <p className="error small">{err}</p>}
      {sum && (
        <>
          <table className="small">
            <thead>
              <tr>
                <th>{sum.group_by}</th>
                <th>cells</th>
                <th>median</th>
                <th className="box-col">p05 · IQR · p95</th>
              </tr>
            </thead>
            <tbody>
              {groups.map((g) => (
                <tr key={g.name} title={`mean ${g.mean.toFixed(4)} ± ${g.sd.toFixed(4)} (sd); q1 ${g.q1.toFixed(4)}, q3 ${g.q3.toFixed(4)}`}>
                  <td className="name">{g.name}</td>
                  <td>{g.n.toLocaleString()}</td>
                  <td>{g.median.toFixed(3)}</td>
                  <td className="box-col">
                    <div className="box">
                      <span className="whisker" style={{ left: x(g.p05), width: `calc(${x(g.p95)} - ${x(g.p05)})` }} />
                      <span className="iqr" style={{ left: x(g.q1), width: `calc(${x(g.q3)} - ${x(g.q1)})` }} />
                      <span className="med" style={{ left: x(g.median) }} />
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          <p className="muted small">
            {ucellLabel(result, result.key)} · join: {sum.join}
            {sum.n_unassigned > 0 && ` · ${sum.n_unassigned.toLocaleString()} cells without a category`}
            {sum.n_no_score > 0 && ` · ${sum.n_no_score.toLocaleString()} without a score`}. Box scale 0 – {hi.toFixed(3)}.
          </p>
        </>
      )}
    </div>
  );
}
