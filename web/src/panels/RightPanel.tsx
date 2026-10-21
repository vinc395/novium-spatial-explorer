import { useEffect, useState } from "react";
import { activePanel, getState, morphSettings, savePref, setCategoryColor, setPalette, setPanel, setState, useStore } from "../store";
import { CATEGORICAL_SCHEMES, isDefaultChoice, rgbaHex } from "../palettes";
import { exportLegendOnly } from "../export/exportView";
import type { SectionApi } from "../api";
import { isLr, LR_GROUPS, lrRows, type LrRow } from "../lr";
import type { FieldInfo } from "../types";
import { COLORMAPS, cssGradient, rgbaCss } from "../colors";
import { gridPaletteKey, recomputeColors, type Legend } from "../data";
import { viewerApi } from "../viewer/Viewer";
import { Collapsible } from "./Collapsible";

interface LegendProps {
  legend: Legend;
  /** Hidden category codes and a setter; without them the category list is read-only. */
  /** hidden category names (shared by all panels) */
  hidden?: Set<string>;
  onHidden?: (h: Set<string>) => void;
  /** Range setter; when given, the min/max editor is shown (null = auto). */
  onRange?: (r: [number, number] | null) => void;
  /** What a missing value refers to in the footnote ("cells", "grid squares"). */
  unit?: string;
  /** How missing values are drawn, for the footnote. */
  missingAs?: string;
  /** Categorical cell field whose colours can be changed here (scheme, shuffle, per category). */
  paletteKey?: string;
  /** Colour map setter of a numeric legend. */
  onColormap?: (name: string) => void;
  /** Whether cells without a value are drawn (grey), and a setter for it. */
  hideMissing?: boolean;
  onHideMissing?: (hide: boolean) => void;
}

function LegendView(props: LegendProps) {
  const { legend, hidden, onHidden, onRange, paletteKey, onColormap } = props;
  const [q, setQ] = useState("");
  const pal = useStore((s) => (paletteKey ? s.palettes[paletteKey] : undefined));
  if (legend.kind === "none") return <p className="muted">{legend.label}</p>;
  if (legend.kind === "numeric") {
    const [lo, hi] = legend.range!;
    return (
      <div className="legend">
        <div className="legend-title">{legend.label}</div>
        <div className="ramp" style={{ background: cssGradient(legend.colormap!) }} />
        <div className="ramp-labels">
          <span>{fmt(lo)}</span>
          <span className="muted small">{legend.log ? "log1p scale · " : ""}{legend.note}</span>
          <span>{fmt(hi)}</span>
        </div>
        {(onRange || onColormap) && (
          <div className="row small">
            {onRange && (
              <>
                <input type="number" step="any" defaultValue={lo} key={`lo${lo}`} aria-label="min" onBlur={(e) => onRange([Number(e.target.value), hi])} />
                <input type="number" step="any" defaultValue={hi} key={`hi${hi}`} aria-label="max" onBlur={(e) => onRange([lo, Number(e.target.value)])} />
                <button className="link" onClick={() => onRange(null)}>auto</button>
              </>
            )}
            {onColormap && (
              <select value={legend.colormap} onChange={(e) => onColormap(e.target.value)} aria-label="Colour scheme">
                {COLORMAPS.map((c) => (
                  <option key={c}>{c}</option>
                ))}
              </select>
            )}
          </div>
        )}
        <MissingNote {...props} />
        <Provenance legend={legend} />
      </div>
    );
  }
  const all = legend.categories ?? [];
  const cats = all.filter((c) => !q || c.name.toLowerCase().includes(q.toLowerCase()));
  const editable = !!(hidden && onHidden);
  const only = (name: string) => onHidden!(new Set(all.filter((c) => c.name !== name).map((c) => c.name)));
  const toggle = (name: string) => {
    const h = new Set(hidden);
    if (h.has(name)) h.delete(name);
    else h.add(name);
    onHidden!(h);
  };
  const nPicked = Object.keys(pal?.custom ?? {}).length;
  return (
    <div className="legend">
      <div className="legend-title">{legend.label} <span className="muted">({all.length} categories)</span></div>
      {paletteKey && (
        <div className="row small palette-row">
          <select
            value={pal?.scheme ?? "default"}
            onChange={(e) => setPalette(paletteKey, { scheme: e.target.value })}
            aria-label="Colour scheme"
          >
            {Object.entries(CATEGORICAL_SCHEMES).map(([k, v]) => (
              <option key={k} value={k}>
                {v.label}
              </option>
            ))}
          </select>
          <button className="link" onClick={() => setPalette(paletteKey, { seed: 1 + Math.floor(Math.random() * 2 ** 31) })}>
            shuffle
          </button>
          {!isDefaultChoice(pal) && (
            <button className="link" onClick={() => setPalette(paletteKey, null)}>
              reset colours
            </button>
          )}
          {nPicked > 0 && <span className="muted">{nPicked} picked by hand</span>}
        </div>
      )}
      {all.length > 12 && <input type="search" placeholder="Filter categories…" value={q} onChange={(e) => setQ(e.target.value)} />}
      {editable && (
        <div className="row small">
          <button className="link" onClick={() => onHidden!(new Set())}>show all</button>
          <button className="link" onClick={() => onHidden!(new Set(all.map((c) => c.name)))}>hide all</button>
          <span className="muted">
            click: show/hide · shift-click or “only”: just this one{paletteKey ? " · swatch: pick a colour" : ""}
          </span>
        </div>
      )}
      <ul className="cats">
        {cats.map((c) => {
          const off = editable && hidden!.has(c.name);
          return (
            <li key={c.code} className={off ? "off" : ""}>
              {paletteKey && (
                <input
                  type="color"
                  className="sw sw-pick"
                  value={rgbaHex(c.color)}
                  onChange={(e) => setCategoryColor(paletteKey, c.name, e.target.value)}
                  aria-label={`colour of ${c.name}`}
                  title={`pick a colour for ${c.name}`}
                />
              )}
              <button
                className="cat"
                disabled={!editable}
                aria-pressed={editable ? !off : undefined}
                onClick={(e) => (e.shiftKey ? only(c.name) : toggle(c.name))}
                title={editable ? "click to show/hide; shift-click to show only this" : undefined}
              >
                {!paletteKey && <span className="sw" style={{ background: rgbaCss(c.color) }} />}
                <span className="name">{c.name}</span>
                <span className="muted">{c.count.toLocaleString()}</span>
              </button>
              {editable && (
                <button className="link only" onClick={() => only(c.name)} aria-label={`show only ${c.name}`}>
                  only
                </button>
              )}
            </li>
          );
        })}
      </ul>
      <MissingNote {...props} />
      <Provenance legend={legend} />
    </div>
  );
}

/** "N cells without a value" footnote; with a setter, a switch to draw them grey or not at all. */
function MissingNote({ legend, unit = "cells", missingAs = "grey", hideMissing, onHideMissing }: LegendProps) {
  if (!legend.nMissing) return null;
  const n = `${legend.nMissing.toLocaleString()} ${unit} without a value`;
  if (!onHideMissing) return <p className="muted small">{n} ({missingAs})</p>;
  return (
    <label className="check small missing-toggle">
      <input type="checkbox" checked={!hideMissing} onChange={(e) => onHideMissing(!e.target.checked)} /> show the {n} ({missingAs})
    </label>
  );
}

function Provenance({ legend }: { legend: Legend }) {
  const p = legend.field?.provenance;
  if (!legend.field) return null;
  const entries = Object.entries(p ?? {});
  return (
    <p className="muted small">
      source: {legend.field.source_file}
      {entries.map(([k, v]) => ` · ${k}: ${Array.isArray(v) ? v.join(", ") : String(v)}`)}
    </p>
  );
}

/** LR values at the selected cell, fetched when opened: the server reads one value per
 *  column, and a section has hundreds of LR columns. */
function LrAtCell({ api, cellKey, fields }: { api: SectionApi; cellKey: number; fields: FieldInfo[] }) {
  const [open, setOpen] = useState(false);
  const [got, setGot] = useState<{ key: number; rows: LrRow[] }>();
  const [err, setErr] = useState<string>();
  const [all, setAll] = useState(false);
  const nLr = fields.filter((f) => f.group === "lr_score").length;
  const loadedKey = got?.key;
  useEffect(() => {
    if (!open || loadedKey === cellKey) return;
    const ctl = new AbortController();
    setErr(undefined);
    api
      .cell(cellKey, ctl.signal, { groups: LR_GROUPS })
      .then((d) => setGot({ key: cellKey, rows: lrRows(fields, d.fields) }))
      .catch((e) => {
        if (!ctl.signal.aborted) setErr(String(e));
      });
    return () => ctl.abort();
  }, [open, cellKey, api, fields, loadedKey]);
  const rows = got?.key === cellKey ? got.rows : undefined;
  const scored = rows?.filter((r) => r.score !== null) ?? [];
  const hot = scored.filter((r) => r.hotspot?.startsWith("hotspot")).length;
  const shown = rows ? (all ? rows : scored.slice(0, 25)) : [];
  return (
    <details className="lr-at-cell" onToggle={(e) => setOpen(e.currentTarget.open)}>
      <summary>
        LR at this cell{" "}
        <span className="muted small">
          {rows
            ? `${scored.length} of ${nLr} scored here · ${hot} hotspot${hot === 1 ? "" : "s"}`
            : `${nLr} interactions, loaded when opened`}
        </span>
      </summary>
      {err && <p className="error">{err}</p>}
      {open && !rows && !err && <p className="muted small">Loading…</p>}
      {rows && (
        <>
          <table aria-label="LR at this cell">
            <tbody>
              {shown.map((r) => (
                <tr key={r.id}>
                  <th title={r.id}>{r.label}</th>
                  <td>{r.score === null ? <span className="muted">—</span> : fmt(r.score)}</td>
                  <td className="small">{r.hotspot?.startsWith("hotspot") ? r.hotspot : ""}</td>
                </tr>
              ))}
            </tbody>
          </table>
          {rows.length > shown.length && (
            <button className="link" onClick={() => setAll(true)}>
              Show all {rows.length}
            </button>
          )}
        </>
      )}
    </details>
  );
}

function fmt(x: number) {
  return Math.abs(x) >= 1000 || (Math.abs(x) < 0.01 && x !== 0) ? x.toExponential(2) : x.toFixed(2);
}

export function RightPanel() {
  const p = useStore((s) => activePanel(s));
  const panelCount = useStore((s) => s.panels.length);
  const m = p.manifest;
  const legend = p.legend;
  const gridImage = p.gridImage;
  const gridVisible = useStore((s) => s.grid.visible);
  const gridHidden = useStore((s) => s.grid.hidden);
  const gridId = useStore((s) => s.grid.gridId);
  const gridFieldId = useStore((s) => s.grid.fieldId);
  const colorBy = useStore((s) => s.colorBy);
  const overlay = useStore((s) => s.overlay);
  const sample = useStore((s) => s.sections.find((x) => x.section_id === p.sectionId)?.sample);
  const heat = useStore((s) => s.ucell.heat);
  const morph = useStore((s) => s.morph);
  const morphMaster = useStore((s) => s.morphOpacity);
  const morphOn = (m?.images ?? []).filter((i) => i.channel).map((i) => ({ img: i, set: morphSettings(morph, i) })).filter((c) => c.set.on);
  const { details, detailsError, selected, selection, api } = p;
  const foldLegend = useStore((s) => s.foldLegendOnClick);
  const [legendErr, setLegendErr] = useState<string>();
  const [exporting, setExporting] = useState(false);
  const [exportErr, setExportErr] = useState<string>();
  const [exportLr, setExportLr] = useState(false);
  if (!m) return <aside className="panel right" />;
  const fields = m.cells?.fields ?? [];
  const nLrFields = fields.filter(isLr).length;
  const doExport = async () => {
    if (!api) return;
    setExporting(true);
    setExportErr(undefined);
    try {
      const cols = fields.filter((f) => exportLr || !isLr(f)).map((f) => f.id);
      const blob = await api.exportCsv([...selection].sort((a, b) => a - b), cols);
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = `${m.section_id}_selection.csv`;
      a.click();
      setTimeout(() => URL.revokeObjectURL(url), 5000);
    } catch (e) {
      setExportErr(String(e));
    } finally {
      setExporting(false);
    }
  };
  return (
    <aside className="panel right">
      <Collapsible id="legend" title={<>Legend{panelCount > 1 && m ? <span className="muted"> · {m.section_id}</span> : null}</>}>
        <div className="row small legend-tools">
          <button className="link" onClick={() => exportLegendOnly().catch((e) => setLegendErr(String(e instanceof Error ? e.message : e)))} title="Save the legends shown here as one SVG (vector) file">
            save legend as SVG
          </button>
          {legendErr && <span className="error">{legendErr}</span>}
        </div>
        {legend ? (
          <LegendView
            legend={legend}
            onRange={(r) => setState({ colorBy: { ...getState().colorBy, range: r } })}
            hidden={colorBy.hidden}
            onHidden={(h) => setState({ colorBy: { ...getState().colorBy, hidden: h } })}
            paletteKey={legend.kind === "categorical" ? legend.field?.id : undefined}
            onColormap={(c) => setState({ colorBy: { ...getState().colorBy, colormap: c } })}
            hideMissing={colorBy.hideMissing}
            onHideMissing={(h) => setState({ colorBy: { ...getState().colorBy, hideMissing: h } })}
          />
        ) : (
          <p className="muted">Loading…</p>
        )}
        {overlay.source && p.overlayLegend && (
          <div className="overlay-legend" aria-label="Overlay legend">
            <div className="legend-sub">
              Overlay
              {overlay.fill ? ` · fill ${Math.round(overlay.opacity * 100)}%` : ""}
              {overlay.outline ? ` · outline ${overlay.outlineWidth} px` : ""}
              {!overlay.fill && !overlay.outline ? " · hidden (fill and outline off)" : ""}
              {overlay.fadeLow && p.overlayLegend.kind === "numeric" ? " · low values fade" : ""}
            </div>
            <LegendView
              legend={p.overlayLegend}
              missingAs="base colour shows"
              onRange={(r) => setState({ overlay: { ...getState().overlay, range: r } })}
              hidden={overlay.hidden}
              onHidden={(h) => setState({ overlay: { ...getState().overlay, hidden: h } })}
              paletteKey={p.overlayLegend.kind === "categorical" ? p.overlayLegend.field?.id : undefined}
              onColormap={(c) => setState({ overlay: { ...getState().overlay, colormap: c } })}
            />
          </div>
        )}
        {morphOn.length > 0 && (
          <div className="overlay-legend" aria-label="Morphology legend">
            <div className="legend-sub">Morphology (additive) · opacity {Math.round(morphMaster * 100)}%</div>
            <ul className="cats">
              {morphOn.map(({ img, set }) => (
                <li key={img.id}>
                  <span className="cat">
                    <span className="sw" style={{ background: set.color }} />
                    <span className="name">{img.channel!.name}</span>
                    <span className="muted">
                      {set.brightness.toFixed(2)}× · {Math.round(set.opacity * 100)}%
                    </span>
                  </span>
                </li>
              ))}
            </ul>
          </div>
        )}
        {heat.key && heat.visible && (
          <div className="overlay-legend" aria-label="UCell heatmap legend">
            <div className="legend-sub">UCell heatmap · opacity {Math.round(heat.opacity * 100)}%</div>
            {p.ucellImage ? (
              <LegendView legend={p.ucellImage.legend} unit="bins" />
            ) : (
              <p className="muted small">Not scored for {m.section_id} yet (score it under Gene signatures).</p>
            )}
          </div>
        )}
        {gridImage && gridVisible && (
          <LegendView
            legend={gridImage.legend}
            unit="grid squares"
            hidden={gridHidden}
            onHidden={(h) => setState({ grid: { ...getState().grid, hidden: h } })}
            paletteKey={gridImage.legend.kind === "categorical" && gridId && gridFieldId ? gridPaletteKey(gridId, gridFieldId) : undefined}
            onColormap={(c) => setState({ grid: { ...getState().grid, colormap: c } })}
          />
        )}
      </Collapsible>
      <Collapsible id="selected-cell" title={<>Selected cell{panelCount > 1 && m ? <span className="muted"> · {m.section_id}</span> : null}</>}>
        {!selected && <p className="muted">Click a cell (shift-click to build a selection).</p>}
        <label className="check small" title="Make room for the cell's details when you click a cell">
          <input
            type="checkbox"
            checked={foldLegend}
            onChange={(e) => {
              savePref("novium:foldLegendOnClick", e.target.checked);
              setState({ foldLegendOnClick: e.target.checked });
            }}
          />{" "}
          fold the legend when a cell is clicked
        </label>
        {detailsError && <p className="error">{detailsError}</p>}
        {selected && details?.cell_key === selected && (
          <div className="inspector">
            <div className="kv"><span>cell_id</span><code>{details.cell_id}</code></div>
            <div className="kv"><span>centroid</span><span>{details.centroid_um[0].toFixed(2)}, {details.centroid_um[1].toFixed(2)} µm</span></div>
            <button className="link" onClick={() => viewerApi.flyTo?.(details.centroid_um[0], details.centroid_um[1], 4)}>zoom to cell</button>
            <table>
              <tbody>
                {fields.filter((f) => f.id in details.fields).map((f) => {
                  const v = details.fields[f.id];
                  return (
                    <tr key={f.id}>
                      <th title={`${f.id} (${f.source_file})`}>{f.label}</th>
                      <td>{v === null ? <span className="muted">—</span> : typeof v === "number" ? fmt(v) : v}</td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
            {api && nLrFields > 0 && <LrAtCell api={api} cellKey={details.cell_key} fields={fields} />}
            <h4>
              Top features{" "}
              <span className="muted small">
                (QV≥{details.expression.qv_min}: {details.expression.total_genes} gene transcripts
                {details.expression.total_all_features !== details.expression.total_genes &&
                  `, ${details.expression.total_all_features - details.expression.total_genes} control/codeword`}
                )
              </span>
            </h4>
            <ul className="topgenes">
              {details.expression.top.map((g) => (
                <li key={g.gene_id} className={g.is_gene ? "" : "muted"} title={g.is_gene ? undefined : "control / codeword feature"}>
                  <span>{g.gene}</span>
                  <span>{g.count}</span>
                </li>
              ))}
              {details.expression.top.length === 0 && <li className="muted">none</li>}
            </ul>
          </div>
        )}
      </Collapsible>
      <Collapsible id="selection" title={`Selection (${selection.size.toLocaleString()})`}>
        <div className="row">
          <button disabled={!selection.size || exporting} onClick={doExport}>{exporting ? "Exporting…" : "Export CSV"}</button>
          <button disabled={!selection.size} onClick={() => setPanel(p.slot, { selection: new Set() })}>Clear</button>
          <button disabled={!selected} onClick={() => { const s = new Set(selection); s.add(selected!); setPanel(p.slot, { selection: s }); }}>Add current</button>
        </div>
        {nLrFields > 0 && (
          <label className="row small">
            <input type="checkbox" checked={exportLr} onChange={(e) => setExportLr(e.target.checked)} />
            include the {nLrFields} LR columns
          </label>
        )}
        {exportErr && <p className="error">{exportErr}</p>}
        <p className="muted small">
          Export contains original Xenium cell_id, section and centroid (µm) plus all cell fields
          {nLrFields > 0 ? " (LR columns only when ticked)" : ""}.
        </p>
      </Collapsible>
      <Collapsible id="index" title="Index">
        {sample && sample.length > 0 && (
          <table className="sample-info small" aria-label="Sample information">
            <tbody>
              {sample.map(([k, v]) => (
                <tr key={k}>
                  <th>{k}</th>
                  <td>{v}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
        <p className="small">{m.display_name}<br /><span className="muted">{m.created_by} · {m.xenium.analysis_sw_version ?? "unknown XOA"} · {m.cells?.count.toLocaleString()} cells · {m.transcripts?.indexed.toLocaleString() ?? 0} transcripts</span></p>
        {m.images.map((i) => (
          <p key={i.id} className="small muted">
            {i.label}: {i.registration.mode}, ~{i.registration.approx_pixel_size_um.toFixed(4)} µm/px{i.registration.is_reflection ? ", mirrored" : ""}
          </p>
        ))}
        {m.warnings.length > 0 && (
          <details>
            <summary>{m.warnings.length} import warning(s)</summary>
            <ul className="small">{m.warnings.map((w, i) => <li key={i}>{w}</li>)}</ul>
          </details>
        )}
        <button className="link" onClick={() => recomputeColors()}>refresh colours</button>
      </Collapsible>
    </aside>
  );
}
