import { useEffect, useRef, useState } from "react";
import { activePanel, emptyPanel, geneIdsByName, getState, setSidePanel, setState, subscribe, useStore } from "./store";
import { buildGridImage, buildUcellHeat, closeSlot, loadGeneTileCounts, loadSections, openSection, overlayColorKey, recomputeColors } from "./data";
import { savePalettes } from "./palettes";
import { Viewer, selectCell, viewerApi } from "./viewer/Viewer";
import { LeftPanel } from "./panels/LeftPanel";
import { RightPanel } from "./panels/RightPanel";
import { StatusBar } from "./panels/StatusBar";
import { ExportDialog } from "./panels/ExportDialog";
import { sectionLabel } from "./types";
import { applyTheme, type ThemeChoice } from "./theme";
import { Brand } from "./Brand";
import { DEMO, session } from "./api";

/** The project page linked from the demo badge (set when the demo is built). */
const DEMO_REPO = (import.meta.env.VITE_DEMO_REPO as string | undefined) ?? "";

const MAX_PANELS = 4;

function readUrl() {
  const p = new URLSearchParams(location.search);
  const sections = (p.get("sections") ?? p.get("section") ?? "").split(",").filter(Boolean).slice(0, MAX_PANELS);
  return { sections, genes: (p.get("genes") ?? "").split(",").filter(Boolean) };
}

/** Show `n` panels: new panels open the next section not already shown. */
export function setPanelCount(n: number) {
  const st = getState();
  n = Math.max(1, Math.min(MAX_PANELS, n));
  if (n === st.panels.length) return;
  if (n < st.panels.length) {
    for (const p of st.panels.slice(n)) closeSlot(p.slot);
    setState({ panels: st.panels.slice(0, n), active: Math.min(st.active, n - 1) });
    return;
  }
  const used = new Set(st.panels.map((p) => p.slot));
  const shown = new Set(st.panels.map((p) => p.sectionId));
  const added = [];
  for (let slot = 0; added.length < n - st.panels.length; slot++) if (!used.has(slot)) added.push(emptyPanel(slot));
  setState({ panels: [...st.panels, ...added] });
  const candidates = st.sections.filter((s) => s.version && !shown.has(s.section_id));
  added.forEach((p, i) => {
    const sec = candidates[i] ?? st.sections.find((s) => s.version);
    if (sec) openSection(p.slot, sec.section_id);
  });
}

export function App() {
  const sections = useStore((s) => s.sections);
  const sectionsError = useStore((s) => s.sectionsError);
  const notice = useStore((s) => s.notice);
  const theme = useStore((s) => s.theme);
  const lasso = useStore((s) => s.lasso);
  const sidePanels = useStore((s) => s.sidePanels);
  const [exportOpen, setExportOpen] = useState(false);
  const panelCount = useStore((s) => s.panels.length);
  const linkZoom = useStore((s) => s.linkZoom);
  const active = useStore((s) => activePanel(s));
  const m = active.manifest;
  const [cellQuery, setCellQuery] = useState("");
  const [cellErr, setCellErr] = useState<string>();
  const [signedIn, setSignedIn] = useState(false);
  const [user, setUser] = useState<string>();
  const pendingGenes = useRef<string[]>(readUrl().genes);

  useEffect(() => {
    // Older servers have no session endpoint: no "Sign out" then.
    session().then(
      (s) => {
        setSignedIn(s.auth);
        setUser(s.user ?? undefined);
      },
      () => setSignedIn(false),
    );
    loadSections().then((secs) => {
      const want = readUrl().sections.filter((id) => secs.some((s) => s.section_id === id && s.version));
      const ids = want.length ? want : secs.filter((s) => s.version).slice(0, 1).map((s) => s.section_id);
      if (!ids.length) return;
      setState({ panels: ids.map((_, i) => emptyPanel(i)), active: 0 });
      ids.forEach((id, i) => openSection(i, id));
    });
  }, []);

  // Derived-data reactions outside the render path. `prev` is updated before any
  // side effect because the effects call setState, which re-enters this listener.
  useEffect(() => {
    let prev = getState();
    return subscribe(() => {
      const s = getState();
      const old = prev;
      prev = s;
      const usesUcell = s.colorBy.source.startsWith("ucell:") || s.overlay.source.startsWith("ucell:");
      if (
        s.colorBy !== old.colorBy ||
        overlayColorKey(s.overlay) !== overlayColorKey(old.overlay) ||
        s.palettes !== old.palettes ||
        (usesUcell && s.ucell.results !== old.ucell.results)
      )
        recomputeColors();
      if (s.palettes !== old.palettes) savePalettes(s.palettes);
      if (s.ucell.heat !== old.ucell.heat || s.ucell.results !== old.ucell.results) buildUcellHeat();
      if (s.grid !== old.grid || s.palettes !== old.palettes) buildGridImage();
      if (s.genes !== old.genes) {
        const before = new Set(old.genes.map((g) => g.name));
        for (const g of s.genes) if (!before.has(g.name)) loadGeneTileCounts(g.name);
      }
      const sectionsOf = (x: typeof s) => x.panels.map((p) => p.sectionId ?? "").join(",");
      if (s.genes !== old.genes || sectionsOf(s) !== sectionsOf(old)) {
        const p = new URLSearchParams();
        const ids = s.panels.map((x) => x.sectionId).filter(Boolean) as string[];
        if (ids.length === 1) p.set("section", ids[0]);
        else if (ids.length > 1) p.set("sections", ids.join(","));
        if (s.genes.length) p.set("genes", s.genes.map((g) => g.name).join(","));
        history.replaceState(null, "", `${location.pathname}?${p}`);
      }
    });
  }, []);

  // Restore genes from the URL once a manifest is available.
  useEffect(() => {
    if (!m?.transcripts || !pendingGenes.current.length) return;
    const names = pendingGenes.current;
    pendingGenes.current = [];
    const idx = geneIdsByName(m);
    const missing = names.filter((n) => !idx.has(n));
    if (missing.length) setState({ notice: `Not in this section's panel: ${missing.join(", ")}` });
    const palette = ["#e6194b", "#3cb44b", "#4363d8", "#f58231", "#911eb4", "#42d4f4", "#f032e6", "#bfef45"];
    setState({ genes: names.filter((n) => idx.has(n)).slice(0, 32).map((name, i) => ({ name, color: palette[i % palette.length] })) });
  }, [m]);

  const findCell = async () => {
    const api = activePanel().api;
    if (!api || !cellQuery.trim()) return;
    setCellErr(undefined);
    try {
      const r = await api.lookup(cellQuery.trim());
      viewerApi.flyTo?.(r.centroid_um[0], r.centroid_um[1], 4);
      selectCell(r.cell_key);
    } catch {
      setCellErr("not found");
    }
  };

  return (
    <div className={`app${sidePanels.left ? "" : " hide-left"}${sidePanels.right ? "" : " hide-right"}`}>
      <header>
        <button
          className="panel-toggle"
          aria-pressed={sidePanels.left}
          aria-label="Left panel"
          onClick={() => setSidePanel("left", !sidePanels.left)}
          title="Show or hide the left panel ([)"
        >
          ◧
        </button>
        <Brand />
        {DEMO && (
          <a className="demo-badge" href={DEMO_REPO || undefined} target="_blank" rel="noreferrer" title="An online demo with synthetic (not biological) data. Get NOVIUM Spatial Explorer to view your own Xenium data.">
            Demo · synthetic data
          </a>
        )}
        <select
          value={active.sectionId ?? ""}
          onChange={(e) => openSection(active.slot, e.target.value)}
          aria-label="Section"
          disabled={!sections.length}
          title={panelCount > 1 ? "Section of the active panel" : undefined}
        >
          {!sections.length && <option value="">{sectionsError ? "API unavailable" : "no sections"}</option>}
          {sections.map((s) => (
            <option key={s.section_id} value={s.section_id} disabled={!s.version}>
              {sectionLabel(s)}
              {s.error ? " (unavailable)" : ""}
            </option>
          ))}
        </select>
        <label className="inline small" title="Show several sections side by side">
          Panels
          <select value={panelCount} onChange={(e) => setPanelCount(Number(e.target.value))} aria-label="Number of panels">
            {[1, 2, 3, 4].map((n) => (
              <option key={n} value={n}>
                {n}
              </option>
            ))}
          </select>
        </label>
        {panelCount > 1 && (
          <label className="check small" title="All panels share one zoom level (panning stays per panel)">
            <input type="checkbox" checked={linkZoom} onChange={(e) => setState({ linkZoom: e.target.checked })} /> link zoom
          </label>
        )}
        <span className="search">
          <input
            type="search"
            placeholder="Find cell_id…"
            value={cellQuery}
            onChange={(e) => setCellQuery(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && findCell()}
            aria-label="Find cell by original Xenium cell_id"
          />
          {cellErr && <span className="error small">{cellErr}</span>}
        </span>
        <button onClick={() => viewerApi.fit?.()} title="Fit section (F)">
          Fit
        </button>
        <button
          className={lasso ? "toggle on" : "toggle"}
          aria-pressed={lasso}
          onClick={() => setState({ lasso: !lasso })}
          title="Lasso select (L): drag around cells. Shift adds, Alt removes, Esc stops."
        >
          Lasso
        </button>
        <button onClick={() => setExportOpen(true)} title="Save the panel windows as an image (PNG, JPEG, TIFF or PDF, up to 4× resolution), with the legend as a second file">
          Export
        </button>
        <span className="muted small grow">{m ? `${m.section_id} · world frame: Xenium µm` : ""}</span>
        <label className="small theme-pick" title="Colour theme (System follows your computer's setting)">
          <span className="muted">Theme</span>
          <select
            value={theme}
            onChange={(e) => {
              const t = e.target.value as ThemeChoice;
              applyTheme(t);
              setState({ theme: t });
            }}
            aria-label="Theme"
          >
            <option value="system">System</option>
            <option value="light">Light</option>
            <option value="dark">Dark</option>
          </select>
        </label>
        <button
          className="panel-toggle"
          aria-pressed={sidePanels.right}
          aria-label="Right panel"
          onClick={() => setSidePanel("right", !sidePanels.right)}
          title="Show or hide the right panel (])"
        >
          ◨
        </button>
        {signedIn && (
          <form method="post" action="/logout" className="signout">
            <button title={`${user ? `Signed in as ${user}. ` : ""}Sign out on this computer; the sign-in page appears next time`}>Sign out</button>
          </form>
        )}
        {notice && (
          <span className="notice small" role="status">
            {notice}{" "}
            <button className="link" aria-label="dismiss" onClick={() => setState({ notice: undefined })}>
              ×
            </button>
          </span>
        )}
        {sectionsError && <span className="error small">{sectionsError}</span>}
      </header>
      <LeftPanel />
      <main>
        <Viewer />
      </main>
      <RightPanel />
      <StatusBar />
      {exportOpen && <ExportDialog onClose={() => setExportOpen(false)} />}
    </div>
  );
}
