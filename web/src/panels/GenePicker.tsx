import { useMemo, useState } from "react";
import { activePanel, geneIdsByName, getState, setState, useStore } from "../store";
import { loadGeneTileCounts, nextGeneColor } from "../data";

const MAX_GENES = 32;

/** Genes are shared by name across panels; the list searched is the active panel's panel. */
export function GenePicker() {
  const p = useStore((s) => activePanel(s));
  const panels = useStore((s) => s.panels);
  const genes = useStore((s) => s.genes);
  const [q, setQ] = useState("");
  const [controls, setControls] = useState(false);
  const m = p.manifest;
  const all = m?.transcripts?.genes ?? [];
  const matches = useMemo(() => {
    const t = q.trim().toLowerCase();
    if (!t) return [];
    return all
      .filter((g) => (controls || g.category === "gene") && g.name.toLowerCase().includes(t))
      .sort((a, b) => Number(!a.name.toLowerCase().startsWith(t)) - Number(!b.name.toLowerCase().startsWith(t)) || a.name.localeCompare(b.name))
      .slice(0, 40);
  }, [q, all, controls]);
  if (!m?.transcripts) return <p className="muted">No transcripts indexed for this section.</p>;
  const add = (name: string) => {
    if (genes.some((g) => g.name === name) || genes.length >= MAX_GENES) return;
    setState({ genes: [...getState().genes, { name, color: nextGeneColor() }] });
    setQ("");
    loadGeneTileCounts(name);
  };
  const byName = new Map(all.map((g) => [g.name, g]));
  return (
    <div className="gene-picker">
      <input
        type="search"
        placeholder={`Search ${all.filter((g) => g.category === "gene").length} genes…`}
        value={q}
        onChange={(e) => setQ(e.target.value)}
        onKeyDown={(e) => e.key === "Enter" && matches[0] && add(matches[0].name)}
        aria-label="Search genes"
      />
      <label className="check small">
        <input type="checkbox" checked={controls} onChange={(e) => setControls(e.target.checked)} /> include control features
      </label>
      {matches.length > 0 && (
        <ul className="dropdown" role="listbox">
          {matches.map((g) => (
            <li key={g.id} role="option" aria-selected={false} onMouseDown={() => add(g.name)}>
              <span>{g.name}</span>
              {g.category !== "gene" && <span className="tag">{g.category.replace(/_/g, " ")}</span>}
              <span className="muted">{g.count.toLocaleString()}</span>
            </li>
          ))}
        </ul>
      )}
      <ul className="chips">
        {genes.map((g) => {
          const info = byName.get(g.name);
          const missingIn = panels.filter((x) => x.manifest && !geneIdsByName(x.manifest).has(g.name)).map((x) => x.sectionId);
          return (
            <li key={g.name}>
              <input
                type="color"
                value={g.color}
                aria-label={`colour for ${g.name}`}
                onChange={(e) => setState({ genes: getState().genes.map((x) => (x.name === g.name ? { ...x, color: e.target.value } : x)) })}
              />
              <span
                title={
                  info
                    ? `${info.count.toLocaleString()} transcripts (${info.count_qv20.toLocaleString()} with QV≥20) in ${m.section_id}`
                    : `not in ${m.section_id}`
                }
                className={info ? "" : "muted"}
              >
                {g.name}
                {missingIn.length > 0 && <span className="muted small"> (not in {missingIn.join(", ")})</span>}
              </span>
              <button
                className="link"
                title="Colour cells by this gene's expression"
                onClick={() => setState({ colorBy: { ...getState().colorBy, source: `expr:${g.name}`, range: null, hidden: new Set() } })}
              >
                ◐
              </button>
              <button className="link" aria-label={`remove ${g.name}`} onClick={() => setState({ genes: getState().genes.filter((x) => x.name !== g.name) })}>
                ×
              </button>
            </li>
          );
        })}
      </ul>
      {genes.length >= MAX_GENES && <p className="muted">Maximum of {MAX_GENES} genes at once.</p>}
    </div>
  );
}
