import { useMemo } from "react";
import { activePanel, getState, morphSettings, setState, useStore, type AppState, type MorphChannel } from "../store";
import { COLORMAPS } from "../colors";
import { smoothFactor } from "../gridSmooth";
import { GenePicker } from "./GenePicker";
import { SignaturesPanel } from "./SignaturesPanel";
import { SourcePicker, type PickerGroup } from "./SourcePicker";
import type { GridInfo, Manifest } from "../types";

function upd<K extends keyof AppState>(k: K, patch: Partial<AppState[K]>) {
  setState({ [k]: { ...(getState()[k] as object), ...patch } } as Partial<AppState>);
}

function Slider(p: { label: string; value: number; min: number; max: number; step: number; onChange: (v: number) => void; fmt?: (v: number) => string }) {
  return (
    <label className="slider">
      <span>{p.label}</span>
      <input type="range" min={p.min} max={p.max} step={p.step} value={p.value} onChange={(e) => p.onChange(Number(e.target.value))} />
      <span className="val">{p.fmt ? p.fmt(p.value) : p.value}</span>
    </label>
  );
}

export function LeftPanel() {
  const m = useStore((s) => activePanel(s).manifest);
  const he = useStore((s) => s.he);
  const cells = useStore((s) => s.cells);
  const tx = useStore((s) => s.transcripts);
  const cb = useStore((s) => s.colorBy);
  const grid = useStore((s) => s.grid);
  const ov = useStore((s) => s.overlay);
  if (!m) return <aside className="panel left" />;
  const curGrid = m.grids.find((g) => g.id === grid.gridId);
  const numeric = isNumeric(m, cb.source);
  const ovNumeric = isNumeric(m, ov.source);
  const setOv = (patch: Partial<AppState["overlay"]>) => setState({ overlay: { ...getState().overlay, ...patch } });
  return (
    <aside className="panel left">
      <section>
        <h3>Image</h3>
        {!m.images.some((i) => !i.channel) ? (
          <p className="muted">No registered image in this index.</p>
        ) : (
          <>
            <label className="check">
              <input type="checkbox" checked={he.visible} onChange={(e) => upd("he", { visible: e.target.checked })} aria-label="Show image" />
              <span>{m.images.find((i) => !i.channel)!.label}</span>
            </label>
            <Slider label="Opacity" value={he.opacity} min={0} max={1} step={0.05} onChange={(v) => upd("he", { opacity: v })} />
          </>
        )}
      </section>
      <MorphologySection />
      <section>
        <h3>Cells</h3>
        <div className="row">
          <label className="check"><input type="checkbox" checked={cells.fill} onChange={(e) => upd("cells", { fill: e.target.checked })} disabled={!m.cells?.boundaries} /> fill</label>
          <label className="check"><input type="checkbox" checked={cells.outline} onChange={(e) => upd("cells", { outline: e.target.checked })} disabled={!m.cells?.boundaries} /> outline</label>
          <label className="check"><input type="checkbox" checked={cells.nuclei} onChange={(e) => upd("cells", { nuclei: e.target.checked })} disabled={!m.cells?.nuclei} /> nuclei</label>
        </div>
        <label className="inline">
          centroids
          <select value={cells.centroids} onChange={(e) => upd("cells", { centroids: e.target.value as "auto" })}>
            <option value="auto">auto (zoomed out, with fill)</option>
            <option value="on">always</option>
            <option value="off">never</option>
          </select>
        </label>
        <Slider label="Fill opacity" value={cells.opacity} min={0} max={1} step={0.05} onChange={(v) => upd("cells", { opacity: v })} />
        <Slider label="Outline px" value={cells.outlineWidth} min={0.5} max={4} step={0.5} onChange={(v) => upd("cells", { outlineWidth: v })} />
        {!m.cells?.boundaries && <p className="muted">Cell boundaries not available; centroids only.</p>}
      </section>
      <section>
        <h3>Colour cells by</h3>
        <SourceSelect
          value={cb.source}
          onChange={(v) => setState({ colorBy: { ...cb, source: v, range: null, hidden: new Set() } })}
          label="Colour cells by"
          none="— single colour —"
        />
        <NoLrHint />
        {numeric && (
          <div className="row">
            <select value={cb.colormap} onChange={(e) => setState({ colorBy: { ...cb, colormap: e.target.value } })} aria-label="Colormap">
              {COLORMAPS.map((c) => (
                <option key={c}>{c}</option>
              ))}
            </select>
            <label className="check small"><input type="checkbox" checked={cb.log} onChange={(e) => setState({ colorBy: { ...cb, log: e.target.checked } })} /> log1p</label>
          </div>
        )}
      </section>
      <section>
        <h3>Overlay</h3>
        <SourceSelect
          value={ov.source}
          onChange={(v) => setOv({ source: v, range: null, hidden: new Set() })}
          label="Overlay"
          none="— none —"
        />
        {ov.source && (
          <>
            <div className="row">
              <label className="check"><input type="checkbox" checked={ov.fill} onChange={(e) => setOv({ fill: e.target.checked })} aria-label="Overlay fill" /> fill</label>
              <label className="check"><input type="checkbox" checked={ov.outline} onChange={(e) => setOv({ outline: e.target.checked })} aria-label="Overlay outline" /> outline</label>
              {ovNumeric && (
                <select value={ov.colormap} onChange={(e) => setOv({ colormap: e.target.value })} aria-label="Overlay colormap">
                  {COLORMAPS.map((c) => (
                    <option key={c}>{c}</option>
                  ))}
                </select>
              )}
            </div>
            <Slider label="Fill opacity" value={ov.opacity} min={0} max={1} step={0.05} onChange={(v) => setOv({ opacity: v })} />
            <Slider label="Outline px" value={ov.outlineWidth} min={0.5} max={6} step={0.5} onChange={(v) => setOv({ outlineWidth: v })} />
            {ovNumeric && (
              <div className="row">
                <label className="check small" title="Opacity follows the value: low values fade out, so hotspots stand out">
                  <input type="checkbox" checked={ov.fadeLow} onChange={(e) => setOv({ fadeLow: e.target.checked })} /> fade low values
                </label>
                <label className="check small"><input type="checkbox" checked={ov.log} onChange={(e) => setOv({ log: e.target.checked })} /> log1p</label>
              </div>
            )}
            <p className="muted small">
              Drawn as its own layer above “Colour cells by”. Cells without a value (e.g. LR not estimable) show only the base colour; zoomed out, each
              cell dot gets a smaller overlay dot. Tip: base fill + overlay outline keeps both readable.
            </p>
          </>
        )}
      </section>
      <SignaturesPanel />
      <section>
        <h3>Transcripts</h3>
        <GenePicker />
        <label className="check"><input type="checkbox" checked={tx.visible} onChange={(e) => upd("transcripts", { visible: e.target.checked })} /> show transcripts</label>
        <label className="inline">
          detail
          <select value={tx.mode} onChange={(e) => upd("transcripts", { mode: e.target.value as "auto" })}>
            <option value="auto">auto</option>
            <option value="points">always points</option>
            <option value="density">always density</option>
          </select>
        </label>
        <Slider label="QV ≥" value={tx.qvMin} min={0} max={40} step={1} onChange={(v) => upd("transcripts", { qvMin: v })} />
        <Slider label="Point px" value={tx.pointSize} min={1} max={10} step={0.5} onChange={(v) => upd("transcripts", { pointSize: v })} />
        <Slider label="Opacity" value={tx.opacity} min={0.05} max={1} step={0.05} onChange={(v) => upd("transcripts", { opacity: v })} />
        <label className="check"><input type="checkbox" checked={tx.showUnassigned} onChange={(e) => upd("transcripts", { showUnassigned: e.target.checked })} /> include unassigned</label>
      </section>
      <section>
        <h3>Segmentation-free grids</h3>
        {m.grids.length === 0 ? (
          <p className="muted">No niche/programme grids imported.</p>
        ) : (
          <>
            <label className="check">
              <input type="checkbox" checked={grid.visible} onChange={(e) => upd("grid", { visible: e.target.checked })} />
              <select value={grid.gridId} onChange={(e) => upd("grid", { gridId: e.target.value, fieldId: m.grids.find((g) => g.id === e.target.value)?.fields[0]?.id, hidden: new Set() })} aria-label="Grid">
                {m.grids.map((g) => (
                  <option key={g.id} value={g.id}>{g.label}</option>
                ))}
              </select>
            </label>
            <select value={grid.fieldId} onChange={(e) => upd("grid", { fieldId: e.target.value, hidden: new Set() })} aria-label="Grid field">
              {curGrid?.fields.map((f) => (
                <option key={f.id} value={f.id}>{f.label} ({f.kind})</option>
              ))}
            </select>
            <div className="row">
              <select value={grid.colormap} onChange={(e) => upd("grid", { colormap: e.target.value })} aria-label="Grid colormap">
                {COLORMAPS.map((c) => (<option key={c}>{c}</option>))}
              </select>
              <label className="check" title="Cubic-spline interpolation of each label onto about 2 µm pixels; each pixel shows the label with the largest value. Display only: the grid values are unchanged.">
                <input type="checkbox" checked={grid.smooth} onChange={(e) => upd("grid", { smooth: e.target.checked })} /> smooth
              </label>
            </div>
            <Slider label="Opacity" value={grid.opacity} min={0} max={1} step={0.05} onChange={(v) => upd("grid", { opacity: v })} />
            {curGrid && <p className="muted small">{gridNote(curGrid, grid.smooth)}</p>}
          </>
        )}
      </section>
    </aside>
  );
}

/** How the grid is drawn, for the grid section. */
function gridNote(g: GridInfo, smooth: boolean) {
  const F = smooth ? smoothFactor(g.width, g.height, g.spacing_um) : 1;
  const how = F > 1 ? `drawn smoothed onto ${+(g.spacing_um / F).toFixed(2)} µm pixels (cubic spline; display only)` : "drawn as-is, one square per cell";
  return `${g.width}×${g.height} cells of ${g.spacing_um} µm; ${how}; independent of cell segmentation.`;
}

/** Says so when this section has no LR scores, and names the sections that do. */
function NoLrHint() {
  const m = useStore((s) => activePanel(s).manifest);
  const sections = useStore((s) => s.sections);
  if (!m || m.cells?.fields.some((f) => f.group === "lr_score")) return null;
  const withLr = sections.filter((s) => s.field_groups?.lr_score).map((s) => s.section_id);
  if (!withLr.length) return null;
  return (
    <p className="muted small" aria-label="LR availability">
      No LR scores for {m.section_id}. CytoSignal LR scores are available for {withLr.length} section{withLr.length === 1 ? "" : "s"}: {withLr.join(", ")} (marked “· LR” in the section menus).
    </p>
  );
}

/** Fluorescence channels of the active section: on/off, colour, brightness, opacity. */
function MorphologySection() {
  const m = useStore((s) => activePanel(s).manifest);
  const morph = useStore((s) => s.morph);
  const master = useStore((s) => s.morphOpacity);
  const chans = m?.images.filter((i) => i.channel) ?? [];
  if (!chans.length) return null;
  const set = (img: (typeof chans)[number], patch: Partial<MorphChannel>) => {
    const cur = getState().morph;
    const name = img.channel!.name;
    setState({ morph: { ...cur, [name]: { ...morphSettings(cur, img), ...patch } } });
  };
  const nOn = chans.filter((img) => morphSettings(morph, img).on).length;
  const setAll = (on: boolean) => {
    const cur = getState().morph;
    const next = { ...cur };
    for (const img of chans) next[img.channel!.name] = { ...morphSettings(cur, img), on };
    setState({ morph: next });
  };
  return (
    <section aria-label="Morphology">
      <h3>Morphology</h3>
      <label className="check" title="Show or hide every channel at once">
        <input
          type="checkbox"
          checked={nOn === chans.length}
          ref={(el) => {
            if (el) el.indeterminate = nOn > 0 && nOn < chans.length;
          }}
          onChange={(e) => setAll(e.target.checked)}
          aria-label="Show all channels"
        />
        <span className="name">All channels</span>
      </label>
      <div title="Opacity of all shown channels together (each channel keeps its own opacity)">
        <Slider label="Master opacity" value={master} min={0} max={1} step={0.05} onChange={(v) => setState({ morphOpacity: v })} />
      </div>
      {chans.map((img) => {
        const c = morphSettings(morph, img);
        const name = img.channel!.name;
        return (
          <div className="morph-ch" key={img.id} aria-label={`Channel ${name}`}>
            <div className="row">
              <label className="check">
                <input type="checkbox" checked={c.on} onChange={(e) => set(img, { on: e.target.checked })} aria-label={`Show ${name}`} />
                <span className="name">{name}</span>
              </label>
              <input type="color" value={c.color} onChange={(e) => set(img, { color: e.target.value, on: true })} aria-label={`${name} colour`} />
            </div>
            {c.on && (
              <>
                <Slider label="Brightness" value={c.brightness} min={0.25} max={8} step={0.05} onChange={(v) => set(img, { brightness: v })} fmt={(v) => `${v.toFixed(2)}×`} />
                <Slider label="Opacity" value={c.opacity} min={0} max={1} step={0.05} onChange={(v) => set(img, { opacity: v })} />
              </>
            )}
          </div>
        );
      })}
      <p className="muted small">Xenium morphology stains on the cell coordinates, mixed additively. Untick the H&E image for a dark background.</p>
    </section>
  );
}

function isNumeric(m: Manifest, source: string) {
  return source.startsWith("expr:") || source.startsWith("ucell:") || m.cells?.fields.find((f) => f.id === source)?.kind === "numeric";
}

/** Field / gene-expression / UCell picker shared by "Colour cells by" and "Overlay". */
function SourceSelect(p: { value: string; onChange: (v: string) => void; label: string; none: string }) {
  const m = useStore((s) => activePanel(s).manifest);
  const genes = useStore((s) => s.genes);
  const ucell = useStore((s) => (m ? s.ucell.results[m.section_id] : undefined));
  const groups = useMemo<PickerGroup[]>(() => {
    const fields = m?.cells?.fields ?? [];
    const out: PickerGroup[] = [...new Set(fields.map((f) => f.group))].map((g) => ({
      key: g,
      label: groupLabel(g),
      options: fields
        .filter((f) => f.group === g)
        .map((f) => ({ value: f.id, label: `${f.label}${f.kind === "categorical" ? ` (${f.categories?.length ?? 0})` : ""}` })),
    }));
    if (genes.length)
      out.push({ key: "expr", label: "Gene expression (QV≥20 counts)", options: genes.map((g) => ({ value: `expr:${g.name}`, label: g.name })) });
    const cellScores = ucell?.filter((r) => r.level === "cell") ?? [];
    const uOpts = cellScores.map((r) => ({ value: `ucell:${r.key}`, label: `${r.signature.name} (maxRank ${r.coverage.max_rank_effective})` }));
    if (p.value.startsWith("ucell:") && !uOpts.some((o) => o.value === p.value)) uOpts.push({ value: p.value, label: "UCell (not scored for this section)" });
    if (uOpts.length) out.push({ key: "ucell", label: "UCell scores (cell level)", options: uOpts });
    return out;
  }, [m, genes, ucell, p.value]);
  return <SourcePicker label={p.label} value={p.value} none={p.none} groups={groups} onChange={p.onChange} />;
}

function groupLabel(g: string) {
  return (
    ({
      cells: "Xenium cell summary",
      annotation: "Annotations",
      lr_score: "LR scores (precomputed)",
      lr_hotspot: "LR hotspots (precomputed)",
      bundle: "Imported columns",
    }) as Record<string, string>
  )[g] ?? g;
}
