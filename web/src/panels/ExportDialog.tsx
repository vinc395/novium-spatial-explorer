// "Export view": the panel windows as an image (format, resolution, DPI) and their legend
// as a second file. The last choices are remembered in this browser.

import { useEffect, useState } from "react";
import { exportView, FORMATS, loadExportOptions, saveExportOptions, type ExportFormat, type ExportOptions } from "../export/exportView";
import { useStore } from "../store";
import { MAX_EXPORT_SIDE, PANEL_LAYOUT } from "../viewer/Viewer";

export function ExportDialog({ onClose }: { onClose: () => void }) {
  const [o, setO] = useState<ExportOptions>(loadExportOptions);
  const [busy, setBusy] = useState<string>();
  const [err, setErr] = useState<string>();
  const [saved, setSaved] = useState<string[]>();
  const panelCount = useStore((s) => s.panels.length);
  const set = (patch: Partial<ExportOptions>) => {
    setSaved(undefined);
    setO((cur) => ({ ...cur, ...patch }));
  };
  useEffect(() => {
    const h = (e: KeyboardEvent) => e.key === "Escape" && !busy && onClose();
    window.addEventListener("keydown", h);
    return () => window.removeEventListener("keydown", h);
  }, [busy, onClose]);

  // Output size: the canvas in device pixels × the scale (capped like the capture).
  const canvas = document.querySelector<HTMLCanvasElement>(".viewer canvas");
  const dpr = window.devicePixelRatio || 1;
  const cssW = canvas?.clientWidth ?? 0, cssH = canvas?.clientHeight ?? 0;
  const scale = cssW ? Math.max(1, Math.min(o.scale, MAX_EXPORT_SIDE / (Math.max(cssW, cssH) * dpr))) : o.scale;
  const [cols, rows] = PANEL_LAYOUT[panelCount] ?? [1, 1];
  const part = o.panels === "active" && panelCount > 1;
  const pw = Math.round((cssW * dpr * scale) / (part ? cols : 1));
  const ph = Math.round((cssH * dpr * scale) / (part ? rows : 1));
  const mm = (px: number) => Math.round((px / o.dpi) * 25.4);
  const alphaOk = FORMATS[o.format].alpha;

  const run = async () => {
    setErr(undefined);
    setSaved(undefined);
    saveExportOptions(o);
    try {
      setBusy("Rendering…");
      setSaved(await exportView(o, setBusy));
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(undefined);
    }
  };

  return (
    <div className="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && !busy && onClose()}>
      <div className="modal" role="dialog" aria-modal="true" aria-label="Export view">
        <h2>Export view</h2>
        <p className="muted small">The panel windows only, without the side panels; the legend comes as a second file.</p>
        <label>
          Format
          <select value={o.format} onChange={(e) => set({ format: e.target.value as ExportFormat })} aria-label="Format">
            {Object.entries(FORMATS).map(([k, f]) => (
              <option key={k} value={k}>
                {f.label}
              </option>
            ))}
          </select>
        </label>
        <label>
          Resolution
          <select value={o.scale} onChange={(e) => set({ scale: Number(e.target.value) })} aria-label="Resolution">
            <option value={1}>1× (as on screen)</option>
            <option value={2}>2× (high)</option>
            <option value={3}>3×</option>
            <option value={4}>4× (ultra)</option>
          </select>
        </label>
        <label>
          DPI
          <select value={o.dpi} onChange={(e) => set({ dpi: Number(e.target.value) })} aria-label="DPI">
            {[72, 150, 300, 600].map((d) => (
              <option key={d} value={d}>
                {d} dpi{d === 300 ? " (print)" : d === 72 ? " (screen)" : ""}
              </option>
            ))}
          </select>
        </label>
        <p className="small export-size" aria-label="Output size">
          {pw.toLocaleString()} × {ph.toLocaleString()} px · {mm(pw)} × {mm(ph)} mm at {o.dpi} dpi
          {scale < o.scale && <span className="muted"> (limited to {MAX_EXPORT_SIDE.toLocaleString()} px)</span>}
        </p>
        {panelCount > 1 && (
          <label>
            Panels
            <select value={o.panels} onChange={(e) => set({ panels: e.target.value as ExportOptions["panels"] })} aria-label="Panels">
              <option value="all">all panels</option>
              <option value="active">active panel only</option>
            </select>
          </label>
        )}
        <label>
          Background
          <select value={o.background} onChange={(e) => set({ background: e.target.value as ExportOptions["background"] })} aria-label="Background">
            <option value="theme">as on screen</option>
            <option value="white">white</option>
            <option value="black">black</option>
            <option value="transparent" disabled={!alphaOk}>
              transparent{alphaOk ? "" : " (not in JPEG)"}
            </option>
          </select>
        </label>
        <label>
          Legend file
          <select value={o.legend} onChange={(e) => set({ legend: e.target.value as ExportOptions["legend"] })} aria-label="Legend file">
            <option value="svg">SVG (vector, editable)</option>
            <option value="png">PNG</option>
            <option value="none">none</option>
          </select>
        </label>
        <label className="check">
          <input type="checkbox" checked={o.scaleBar} onChange={(e) => set({ scaleBar: e.target.checked })} /> scale bar
        </label>
        <label className="check">
          <input type="checkbox" checked={o.labels} onChange={(e) => set({ labels: e.target.checked })} /> section ID in each panel
        </label>
        {busy && (
          <p className="muted small" role="status">
            {busy}
          </p>
        )}
        {err && <p className="error small">{err}</p>}
        {saved && (
          <p className="small" role="status">
            Saved {saved.join(" and ")}
          </p>
        )}
        <div className="row modal-actions">
          <button className="primary" disabled={!!busy} onClick={run}>
            Export
          </button>
          <button disabled={!!busy} onClick={onClose}>
            Close
          </button>
        </div>
        <p className="muted small">Exports contain patient-sample images: keep them where your data approvals allow. Your browser may ask once to allow two downloads.</p>
      </div>
    </div>
  );
}
